// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Dino",
    platforms: [.macOS(.v14)],
    dependencies: [
        .package(url: "https://github.com/Lakr233/libghostty-spm.git", exact: "1.6.20260928"),
    ],
    targets: [
        .executableTarget(
            name: "Dino",
            dependencies: [.product(name: "GhosttyTerminal", package: "libghostty-spm")],
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
