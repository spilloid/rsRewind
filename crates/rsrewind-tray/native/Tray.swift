import AppKit

private typealias Snapshot = @convention(c) (UnsafeMutableRawPointer?) -> UnsafePointer<CChar>?
private typealias Click = @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void

private struct Row: Decodable {
    let label: String
    let id: Int32
    let enabled: Bool
    let separator: Bool
}
private struct State: Decodable {
    let headline: String
    let symbol: String
    let recording: Bool
    let warning: Bool
    let rows: [Row]
}

private final class Tray: NSObject, NSMenuDelegate {
    let context: UnsafeMutableRawPointer?
    let snapshot: Snapshot
    let click: Click
    let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    let menu = NSMenu()
    var timer: Timer?
    init(context: UnsafeMutableRawPointer?, snapshot: @escaping Snapshot, click: @escaping Click) {
        self.context = context
        self.snapshot = snapshot
        self.click = click
        super.init()
        menu.autoenablesItems = false
        menu.delegate = self
        item.menu = menu
        refresh()
        // Common mode keeps the status and confirmation expiry fresh during menu tracking.
        let timer = Timer(timeInterval: 0.5, repeats: true) { [weak self] _ in self?.refresh() }
        self.timer = timer
        RunLoop.main.add(timer, forMode: .common)
    }
    func menuWillOpen(_ menu: NSMenu) { refresh() }
    func refresh() {
        guard let raw = snapshot(context), let data = String(cString: raw).data(using: .utf8),
              let state = try? JSONDecoder().decode(State.self, from: data) else { return }
        item.button?.image = NSImage(systemSymbolName: state.symbol, accessibilityDescription: state.headline)
        item.button?.contentTintColor = state.warning ? .systemOrange : (state.recording ? .systemRed : .labelColor)
        item.button?.toolTip = state.headline
        item.button?.setAccessibilityLabel("rsRewind: " + state.headline)
        // Do not replace menu items during tracking; update existing rows in place.
        if menu.items.count != state.rows.count {
            menu.removeAllItems()
            for row in state.rows {
                let entry = row.separator ? NSMenuItem.separator() : NSMenuItem(title: row.label, action: #selector(selected(_:)), keyEquivalent: "")
                entry.target = self
                menu.addItem(entry)
            }
        }
        for (entry, row) in zip(menu.items, state.rows) {
            entry.title = row.label
            entry.tag = Int(row.id)
            entry.isEnabled = row.enabled
        }
    }
    @objc func selected(_ sender: NSMenuItem) {
        click(context, Int32(sender.tag))
        refresh()
    }
    deinit { timer?.invalidate(); NSStatusBar.system.removeStatusItem(item) }
}

@_cdecl("rsrewind_tray_run")
public func runTray(_ context: UnsafeMutableRawPointer?, _ snapshot: @convention(c) (UnsafeMutableRawPointer?) -> UnsafePointer<CChar>?, _ click: @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void) -> Int32 {
    guard Thread.isMainThread else { return 1 }
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    let tray = Tray(context: context, snapshot: snapshot, click: click)
    withExtendedLifetime(tray) { app.run() }
    return 0
}

@_cdecl("rsrewind_tray_quit")
public func quitTray() {
    NSApplication.shared.stop(nil)
    // Wake the run loop so run() returns immediately without terminating capture or workers.
    if let event = NSEvent.otherEvent(with: .applicationDefined, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0, context: nil, subtype: 0, data1: 0, data2: 0) {
        NSApplication.shared.postEvent(event, atStart: true)
    }
}
