// The release key: Ed25519, the scheme Sparkle checks app updates with and dinod checks `dino`
// updates with. The private key is a file holding the base64 of its 32-byte seed, which is also
// what Sparkle's `generate_keys -x` exports, so either tool's key works with the other.
//
//   swift scripts/release-key.swift new FILE      make a key (refuses to overwrite), print its public half
//   swift scripts/release-key.swift public FILE   print the public half (base64), for Info.plist and dinod
//   swift scripts/release-key.swift sign FILE X   print the signature of file X (base64)
import CryptoKit
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("release-key: \(message)\n".utf8))
    exit(1)
}

func load(_ path: String) -> Curve25519.Signing.PrivateKey {
    guard let text = try? String(contentsOfFile: path, encoding: .utf8),
          let raw = Data(base64Encoded: text.trimmingCharacters(in: .whitespacesAndNewlines))
    else { fail("can't read a key in \(path)") }
    // Older Sparkle keys hold the seed and the public key, 64 bytes: the seed comes first.
    guard raw.count == 32 || raw.count == 64, let key = try? Curve25519.Signing.PrivateKey(rawRepresentation: raw.prefix(32))
    else { fail("\(path) isn't an Ed25519 key") }
    return key
}

let args = CommandLine.arguments.dropFirst()
switch (args.first, args.dropFirst().first, args.dropFirst(2).first) {
case ("new", let path?, nil):
    guard !FileManager.default.fileExists(atPath: path) else { fail("\(path) exists; not replacing a release key") }
    let key = Curve25519.Signing.PrivateKey()
    guard FileManager.default.createFile(atPath: path, contents: Data(key.rawRepresentation.base64EncodedString().utf8), attributes: [.posixPermissions: 0o600])
    else { fail("can't write \(path)") }
    print(key.publicKey.rawRepresentation.base64EncodedString())
case ("public", let path?, nil):
    print(load(path).publicKey.rawRepresentation.base64EncodedString())
case ("sign", let path?, let file?):
    guard let data = FileManager.default.contents(atPath: file) else { fail("can't read \(file)") }
    guard let signature = try? load(path).signature(for: data) else { fail("couldn't sign \(file)") }
    print(signature.base64EncodedString())
default:
    fail("usage: release-key.swift new FILE | public FILE | sign FILE FILE")
}
