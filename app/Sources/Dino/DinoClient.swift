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

struct Response: Decodable {
    var type: String
    var sessions: [SessionInfo]?
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
        let resp = try JSONDecoder().decode(Response.self, from: Data(try readExact(n)))
        if resp.type == "error" { throw DinoError.daemon(resp.message ?? "error") }
        return resp
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

    static let socketPath = NSString(string: "~/.config/dino/dinod.sock").expandingTildeInPath

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
