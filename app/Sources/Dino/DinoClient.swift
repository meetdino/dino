import Foundation

/// dinod's wire types (see crates/dino-core/src/ipc.rs).
struct SessionInfo: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var agent_id: String
    var title: String?
    var exited: Bool
    var output_ms_ago: UInt64?
    var bells: UInt64
    var requests: UInt64
    var in_flight: UInt32
    var input_tokens: UInt64
    var output_tokens: UInt64
    var last_model: String?
    var tier: String?
    var activity: String?
    var group: String?
    /// Why the agent's last model call failed.
    var error: String?
    /// Where it runs, symlinks resolved; nil from an older dinod.
    var cwd: String?

    var needs: String? {
        guard let a = activity, a.hasPrefix("needs:") else { return nil }
        return String(a.dropFirst(6))
    }
}

struct WindowInfo: Codable, Equatable {
    var name: String
    var utilization: Float
    var resets_at: UInt64?
}

struct QuotaInfo: Codable, Equatable {
    var provider: String
    var windows: [WindowInfo]
}

struct LauncherInfo: Codable, Identifiable, Equatable {
    var short: String
    var agent_id: String
    var label: String
    var program: String
    var id: String { short }
}

/// A session dino didn't start: running in another terminal, recent on disk, or in the cloud.
struct FoundSession: Codable, Identifiable, Equatable {
    var source: String
    var agent: String
    var session_id: String
    var title: String
    var cwd: String?
    var updated_at: UInt64
    var pid: UInt32?
    var status: String?
    var terminal: String?
    var args: [String]
    var url: String?

    var id: String { "\(source)-\(agent)-\(session_id)-\(pid ?? 0)" }
    var isBusy: Bool { status == "busy" }
}

struct DiffStat: Codable, Equatable {
    var files: UInt32
    var added: UInt32
    var removed: UInt32
}

/// A fan-out: one prompt, several agents, each in its own worktree.
struct GroupInfo: Codable, Identifiable, Equatable {
    var id: String
    var prompt: String
    var repo: String
    var members: [MemberInfo]
}

struct MemberInfo: Codable, Identifiable, Equatable {
    var session: String
    var launcher: String
    var branch: String
    var worktree: String
    var stat: DiffStat?
    var id: String { session }
}

private struct GroupsResponse: Decodable {
    var groups: [GroupInfo]
}

private struct DiffResponse: Decodable {
    var stat: DiffStat
    var text: String
}

private struct FoundResponse: Decodable {
    var sessions: [FoundSession]
}

struct Response: Decodable {
    var type: String
    var sessions: [SessionInfo]?

    enum CodingKeys: String, CodingKey { case type, sessions, quotas, launchers, id, message }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        type = try c.decode(String.self, forKey: .type)
        // `sessions` means SessionInfo only in a state reply.
        sessions = type == "state" ? try c.decodeIfPresent([SessionInfo].self, forKey: .sessions) : nil
        quotas = try c.decodeIfPresent([QuotaInfo].self, forKey: .quotas)
        launchers = try c.decodeIfPresent([LauncherInfo].self, forKey: .launchers)
        id = try c.decodeIfPresent(String.self, forKey: .id)
        message = try c.decodeIfPresent(String.self, forKey: .message)
    }
    var quotas: [QuotaInfo]?
    var launchers: [LauncherInfo]?
    var id: String?
    var message: String?
}

enum DinoError: Error, LocalizedError {
    case socket(String)
    case daemon(String)
    var errorDescription: String? {
        switch self {
        case let .socket(m), let .daemon(m): m
        }
    }
}

/// One connection to dinod speaking the framed protocol: `[kind u8][len u32 BE][payload]`.
final class DinoConnection: @unchecked Sendable {
    private let fd: Int32
    /// One request at a time: the poller and user actions share this connection.
    private let lock = NSLock()

    init(path: String) throws {
        fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw DinoError.socket("socket() failed") }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8CString)
        guard bytes.count <= MemoryLayout.size(ofValue: addr.sun_path) else { throw DinoError.socket("socket path too long") }
        withUnsafeMutableBytes(of: &addr.sun_path) { dst in
            bytes.withUnsafeBytes { dst.copyMemory(from: $0) }
        }
        let ok = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard ok == 0 else {
            close(fd)
            throw DinoError.socket("can't connect to dinod at \(path)")
        }
    }

    deinit { close(fd) }

    func request(_ body: [String: Any]) throws -> Response {
        try JSONDecoder().decode(Response.self, from: send(body))
    }

    func found(cloud: Bool) throws -> [FoundSession] {
        try JSONDecoder().decode(FoundResponse.self, from: send(["type": "found", "cloud": cloud])).sessions
    }

    func groups() throws -> [GroupInfo] {
        try JSONDecoder().decode(GroupsResponse.self, from: send(["type": "groups"])).groups
    }

    func diff(session: String) throws -> String {
        try JSONDecoder().decode(DiffResponse.self, from: send(["type": "diff", "session": session])).text
    }

    /// Continue `session` in dino; returns the new dino session id.
    func adopt(_ session: FoundSession, cwd: String?) throws -> String? {
        let encoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(session))
        var body: [String: Any] = ["type": "adopt", "session": encoded]
        if let cwd { body["cwd"] = cwd }
        return try request(body).id
    }

    /// One request/response exchange; throws dinod's error message as-is.
    func send(_ body: [String: Any]) throws -> Data {
        lock.lock()
        defer { lock.unlock() }
        let payload = try JSONSerialization.data(withJSONObject: body)
        var frame = Data([0])
        var len = UInt32(payload.count).bigEndian
        frame.append(Data(bytes: &len, count: 4))
        frame.append(payload)
        try writeAll(frame)
        let head = try readExact(5)
        let n = Int(UInt32(head[1]) << 24 | UInt32(head[2]) << 16 | UInt32(head[3]) << 8 | UInt32(head[4]))
        let data = Data(try readExact(n))
        if let err = try? JSONDecoder().decode(Response.self, from: data), err.type == "error" {
            throw DinoError.daemon(err.message ?? "error")
        }
        return data
    }

    private func writeAll(_ data: Data) throws {
        try data.withUnsafeBytes { raw in
            var off = 0
            while off < raw.count {
                let n = write(fd, raw.baseAddress! + off, raw.count - off)
                guard n > 0 else { throw DinoError.socket("write failed") }
                off += n
            }
        }
    }

    private func readExact(_ count: Int) throws -> [UInt8] {
        var buf = [UInt8](repeating: 0, count: count)
        var off = 0
        while off < count {
            let n = buf.withUnsafeMutableBytes { read(fd, $0.baseAddress! + off, count - off) }
            guard n > 0 else { throw DinoError.socket("dinod closed the connection") }
            off += n
        }
        return buf
    }
}

/// Locating the `dino` CLI and giving it the user's real environment.
enum DinoEnvironment {
    /// Apps launched from Finder get a minimal PATH; agents live in the login shell's PATH.
    static let loginPath: String = {
        let shell = ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh"
        let p = Process()
        p.executableURL = URL(fileURLWithPath: shell)
        p.arguments = ["-l", "-c", "printf %s \"$PATH\""]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        try? p.run()
        p.waitUntilExit()
        let path = String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        return path.isEmpty ? (ProcessInfo.processInfo.environment["PATH"] ?? "/usr/bin:/bin") : path
    }()

    static let dinoBinary: String = {
        let env = ProcessInfo.processInfo.environment
        if let bin = env["DINO_BIN"] { return bin }
        for dir in loginPath.split(separator: ":") {
            let candidate = "\(dir)/dino"
            if FileManager.default.isExecutableFile(atPath: candidate) { return candidate }
        }
        return NSString(string: "~/.local/bin/dino").expandingTildeInPath
    }()

    /// `$DINO_HOME` points the app at a second, isolated dinod (as it does the CLI).
    static let home = ProcessInfo.processInfo.environment["DINO_HOME"] ?? NSString(string: "~/.config/dino").expandingTildeInPath
    static let socketPath = "\(home)/dinod.sock"

    /// `dino ping` starts dinod (with the login PATH) if it isn't running.
    static func ensureDaemon() throws {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: dinoBinary)
        p.arguments = ["ping"]
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = loginPath
        p.environment = env
        p.standardOutput = FileHandle.nullDevice
        try p.run()
        p.waitUntilExit()
        guard p.terminationStatus == 0 else { throw DinoError.daemon("`dino ping` failed; is \(dinoBinary) installed?") }
    }
}
