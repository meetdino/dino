//
//  AppTerminalView.swift
//  libghostty-spm
//
//  Created by Lakr233 on 2026/3/16.
//

#if !canImport(UIKit) && canImport(AppKit)
    import AppKit
    import GhosttyKit

    @MainActor
    open class AppTerminalView: NSView {
        let core = TerminalSurfaceCoordinator()
        var metalLayer: CAMetalLayer?
        var inputHandler: TerminalKeyEventHandler?

        // Grouped view state, one struct per concern — same convention as
        // the UIKit twin: each state type is defined in the extension file
        // that owns the behavior (+Input, +Lifecycle); the storage lives
        // here because extensions cannot add stored properties.
        var keyEcho: KeyEchoState = .init()
        var focusBridge: FocusBridgeState = .init()
        var accessibilityText: AccessibilityTextCache = .init()

        open weak var delegate: (any TerminalSurfaceViewDelegate)? {
            get { core.delegate }
            set { core.delegate = newValue }
        }

        open var controller: TerminalController? {
            get { core.controller }
            set { core.controller = newValue }
        }

        open var configuration: TerminalSurfaceOptions {
            get { core.configuration }
            set { core.configuration = newValue }
        }

        open func setSurfaceVisible(_ visible: Bool) {
            core.setDisplayVisible(visible)
        }

        /// Adjusts this surface's resize coalescing window without rebuilding
        /// it. Overrides `TerminalSurfaceOptions.resizeThrottleMilliseconds`,
        /// which is the declarative home for the same policy and the one every
        /// platform can reach; pass `nil` to fall back to it.
        open func setResizeThrottle(milliseconds: Double?) {
            core.resizeThrottleInterval = milliseconds.map { max(0, $0) / 1000 }
        }

        var surface: TerminalSurface? {
            core.surface
        }

        override public init(frame: NSRect) {
            super.init(frame: frame)
            commonInit()
        }

        @available(*, unavailable)
        public required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        func commonInit() {
            wantsLayer = true

            let metal = CAMetalLayer()
            metal.device = MTLCreateSystemDefaultDevice()
            metal.pixelFormat = .bgra8Unorm
            metal.framebufferOnly = true
            metal.contentsScale = NSScreen.main?.backingScaleFactor ?? 2.0
            metal.isOpaque = false
            metal.backgroundColor = NSColor.clear.cgColor
            layer = metal
            metalLayer = metal
            layer?.backgroundColor = NSColor.clear.cgColor

            inputHandler = TerminalKeyEventHandler(view: self)
            setupTrackingArea()

            core.isAttached = { [weak self] in self?.window != nil }
            core.hasKeyFocus = { [weak self] in
                guard let self, let window else { return false }
                return window.isKeyWindow && window.firstResponder === self
            }
            core.scaleFactor = { [weak self] in
                Double(
                    self?.window?.backingScaleFactor
                        ?? NSScreen.main?.backingScaleFactor ?? 2.0
                )
            }
            core.viewSize = { [weak self] in
                guard let self else { return (0, 0) }
                return (bounds.width, bounds.height)
            }
            core.platformSetup = { [weak self] config in
                guard let self else { return }
                config.platform_tag = GHOSTTY_PLATFORM_MACOS
                config.platform = ghostty_platform_u(
                    macos: ghostty_platform_macos_s(
                        nsview: Unmanaged.passUnretained(self).toOpaque()
                    )
                )
            }
            core.onMetricsUpdate = { [weak self] in
                self?.updateMetalLayerMetrics()
            }
            core.onPostRender = { [weak self] in
                self?.enforceMetalLayerScale()
            }
        }

        /// The menu a right-click shows when Ghostty leaves the click to the
        /// host (`right-click-action = context-menu`, the default, and no
        /// program capturing the mouse): Ghostty has already selected the
        /// word or link under the pointer, unless the click was inside a
        /// selection. A host overrides this for a menu of its own; nil shows
        /// none. Upstream's: Copy, when there is a selection.
        open func contextMenu() -> NSMenu? {
            guard surface?.hasSelection() == true else { return nil }
            let menu = NSMenu()
            let copyItem = NSMenuItem(
                title: "Copy",
                action: #selector(copy(_:)),
                keyEquivalent: ""
            )
            copyItem.target = self
            menu.addItem(copyItem)
            return menu
        }

        @discardableResult
        open func copySelectedTextToPasteboard() -> Bool {
            guard surface?.hasSelection() == true else {
                return false
            }
            guard surface?.performBindingAction("copy_to_clipboard") == true else {
                return false
            }
            TerminalDebugLog.log(
                .input,
                "selection copied to clipboard"
            )
            return true
        }

        deinit {
            NotificationCenter.default.removeObserver(self)
        }
    }
#endif
