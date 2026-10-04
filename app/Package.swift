// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Dino",
    platforms: [.macOS(.v14)],
    dependencies: [
        // The engine (libghostty, as GhosttyKit) comes from the package; its Swift wrapper,
        // GhosttyTerminal, is carried in Sources/DinoGhostty so dino can answer every action
        // Ghostty sends (see Sources/DinoGhostty/README.md).
        .package(url: "https://github.com/Lakr233/libghostty-spm.git", exact: "1.6.20260928"),
        .package(url: "https://github.com/Lakr233/MSDisplayLink.git", exact: "2.2.0"),
        .package(url: "https://github.com/sparkle-project/Sparkle", exact: "2.10.0"),
    ],
    targets: [
        .target(
            name: "DinoGhostty",
            dependencies: [
                .product(name: "GhosttyKit", package: "libghostty-spm"),
                .product(name: "MSDisplayLink", package: "MSDisplayLink"),
            ],
            path: "Sources/DinoGhostty",
            exclude: ["LICENSE", "README.md"],
            resources: [
                .copy("Resources/Ghostty"),
                .copy("Resources/terminfo"),
            ]
        ),
        .executableTarget(
            name: "Dino",
            dependencies: [
                "DinoGhostty",
                .product(name: "Sparkle", package: "Sparkle"),
            ],
            swiftSettings: [.swiftLanguageMode(.v5)],
            // Sparkle.framework sits in Contents/Frameworks of the app bundle.
            linkerSettings: [.unsafeFlags(["-Xlinker", "-rpath", "-Xlinker", "@executable_path/../Frameworks"])]
        ),
    ]
)
