import AppKit
import Foundation
import UserNotifications
import CryptoKit
import Darwin

private struct InstanceIdentity: Decodable {
    let instance_key: String
    let data_dir: String
    let diagnostics: String
}
private enum LoginOperation: String, Decodable { case enable, disable }
private struct ServiceStatus: Decodable {
    let instance_key: String
    let installed: Bool
    let login_enabled: Bool
    let login_pending: LoginOperation?
    let desired_running: Bool
    let pending_installation: Bool
    let observed: String
    var nextLoginOperation: LoginOperation { login_pending ?? (login_enabled ? .disable : .enable) }
    var loginActionTitle: String {
        if let pending = login_pending { return "Finish \(pending.rawValue) start at login" }
        return login_enabled ? "Disable start at login" : "Enable start at login"
    }
}
private struct AttentionEntry: Decodable {
    let attention_id: String
    let category: String
    let episode_key: String?
}
private struct AttentionSnapshot: Decodable {
    let instance_key: String
    let incarnation: String
    let revision: String
    let count: Int
    let state: String
    let entries: [AttentionEntry]
}
private enum DeliveryState: String, Codable { case baseline, attempted, accepted, observed, failed }
private struct Delivery: Codable {
    let attention: String
    let episode: String
    let requestID: String
    var state: DeliveryState
    var attempts: Int
    var updated: Date
}
private struct Ledger: Codable {
    var needsBaseline = true
    var entries: [String: Delivery] = [:]
}
private enum MenuError: LocalizedError {
    case message(String)
    var errorDescription: String? { if case .message(let text) = self { return text }; return nil }
}

private final class CommandRunner {
    private let queue = DispatchQueue(label: "com.llmrelay.menu.commands")
    private let executable: URL
    init() throws {
        let url = Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/llmrelay")
        let values = try url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])
        guard values.isRegularFile == true, values.isSymbolicLink != true else {
            throw MenuError.message("The bundled LLMRelay executable is missing or was replaced by a link. Reinstall the package.")
        }
        executable = url
    }

    func run(root: String, arguments: [String], completion: @escaping (Result<Data, Error>) -> Void) {
        queue.async {
            let result = Result { try self.invoke(root: root, arguments: arguments) }
            DispatchQueue.main.async { completion(result) }
        }
    }

    private func invoke(root: String, arguments: [String]) throws -> Data {
        guard root.hasPrefix("/"), root.utf8.count <= 4096,
              arguments.count <= 12, arguments.allSatisfy({ $0.utf8.count <= 4096 }) else {
            throw MenuError.message("The instance path or desktop request is invalid.")
        }
        let process = Process()
        let output = Pipe(), errors = Pipe()
        process.executableURL = executable
        process.arguments = ["service", "--data-dir", root] + arguments
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        process.environment = ["HOME": home, "USER": NSUserName(), "LOGNAME": NSUserName(),
            "PATH": "/opt/homebrew/bin:/usr/local/bin:\(home)/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            "TMPDIR": "/private/tmp"]
        process.currentDirectoryURL = FileManager.default.homeDirectoryForCurrentUser
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = output
        process.standardError = errors
        try process.run()
        defer {
            if process.isRunning {
                process.terminate()
                let until = ProcessInfo.processInfo.systemUptime + 0.3
                while process.isRunning && ProcessInfo.processInfo.systemUptime < until { usleep(10_000) }
                if process.isRunning { kill(process.processIdentifier, SIGKILL) }
                process.waitUntilExit()
            }
            try? output.fileHandleForReading.close()
            try? errors.fileHandleForReading.close()
        }
        for handle in [output.fileHandleForReading, errors.fileHandleForReading] {
            let flags = fcntl(handle.fileDescriptor, F_GETFL)
            guard flags >= 0, fcntl(handle.fileDescriptor, F_SETFL, flags | O_NONBLOCK) >= 0 else {
                throw MenuError.message("Could not safely read the bundled command response.")
            }
        }
        let deadline = ProcessInfo.processInfo.systemUptime + 15
        var stdout = Data(), stderr = Data()
        func readAvailable(_ handle: FileHandle, _ data: inout Data, limit: Int) throws {
            var buffer = [UInt8](repeating: 0, count: 4096)
            while true {
                let count = Darwin.read(handle.fileDescriptor, &buffer, buffer.count)
                if count == 0 { return }
                if count < 0 {
                    if errno == EAGAIN || errno == EWOULDBLOCK { return }
                    throw MenuError.message("The command response could not be read.")
                }
                data.append(contentsOf: buffer.prefix(count))
                if data.count > limit { throw MenuError.message("The command response exceeded the desktop size limit.") }
            }
        }
        repeat {
            try readAvailable(output.fileHandleForReading, &stdout, limit: 128 * 1024)
            try readAvailable(errors.fileHandleForReading, &stderr, limit: 16 * 1024)
            if !process.isRunning { break }
            if ProcessInfo.processInfo.systemUptime >= deadline {
                throw MenuError.message("The service command timed out. Its outcome may be uncertain; inspect Status before retrying.")
            }
            usleep(10_000)
        } while true
        process.waitUntilExit()
        try readAvailable(output.fileHandleForReading, &stdout, limit: 128 * 1024)
        try readAvailable(errors.fileHandleForReading, &stderr, limit: 16 * 1024)
        guard process.terminationStatus == 0 else {
            let detail = String(data: stderr, encoding: .utf8) ?? "No diagnostic details were returned."
            throw MenuError.message("The service could not finish that request. \(detail.prefix(1200))")
        }
        return stdout
    }
}

@MainActor
private final class MenuController: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    private var item: NSStatusItem!
    private var runner: CommandRunner?
    private var identity: InstanceIdentity?
    private var selectedRoot = ""
    private var status: ServiceStatus?
    private var busy = false
    private var timer: Timer?
    private var failures = 0
    private var attentionLabel = "Attention unavailable"
    private var notice = "Connecting to the selected instance…"
    private var ledger = Ledger()
    private var ledgerReady = false
    private var ledgerReconciled = false
    private var deliveryInFlight = false
    private var pendingActivation: [String: String]?
    private let center = UNUserNotificationCenter.current()

    func applicationDidFinishLaunching(_ notification: Notification) {
        item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.title = "LLMRelay"
        center.delegate = self
        do {
            runner = try CommandRunner()
            let arguments = CommandLine.arguments
            if let index = arguments.firstIndex(of: "--data-dir") {
                guard index + 1 < arguments.count, arguments[index + 1].hasPrefix("/") else {
                    throw MenuError.message("--data-dir requires an absolute instance path.")
                }
                selectedRoot = arguments[index + 1]
            } else {
                selectedRoot = FileManager.default.homeDirectoryForCurrentUser
                    .appendingPathComponent("Library/Application Support/AgenticJira").path
            }
            rebuildMenu()
            resolveInstance()
        } catch { notice = error.localizedDescription; rebuildMenu() }
    }

    private var preferenceKey: String? { identity.map { "notifications.\($0.instance_key)" } }
    private var notificationsEnabled: Bool { preferenceKey.map { UserDefaults.standard.bool(forKey: $0) } ?? false }

    private func resolveInstance() {
        guard let runner else { return }
        busy = true
        runner.run(root: selectedRoot, arguments: ["resolve"]) { [weak self] result in
            guard let self else { return }
            self.busy = false
            do {
                let identity = try JSONDecoder().decode(InstanceIdentity.self, from: result.get())
                guard identity.data_dir.hasPrefix("/"), identity.instance_key == self.digest(identity.data_dir),
                      identity.diagnostics == URL(fileURLWithPath: identity.data_dir).appendingPathComponent("logs").path else {
                    throw MenuError.message("The bundled command returned a different instance identity.")
                }
                self.identity = identity
                self.selectedRoot = identity.data_dir
                UserDefaults.standard.set(identity.data_dir, forKey: "root.\(identity.instance_key)")
                self.loadLedger()
                self.reconcileNotifications()
                self.poll()
            } catch { self.notice = error.localizedDescription; self.rebuildMenu() }
        }
    }

    private func digest(_ text: String) -> String { SHA256.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined() }
    private func requestID(_ episode: String) -> String { "llmrelay.\(digest((identity?.instance_key ?? "") + ":" + episode))" }

    private func perform(_ arguments: [String], completion: ((Data) throws -> Void)? = nil) {
        guard let runner, let identity, !busy else { return }
        timer?.invalidate()
        busy = true
        rebuildMenu()
        runner.run(root: identity.data_dir, arguments: arguments) { [weak self] result in
            guard let self else { return }
            self.busy = false
            do {
                let data = try result.get()
                let object = try JSONSerialization.jsonObject(with: data) as? [String: Any]
                guard object?["instance_key"] as? String == identity.instance_key,
                      self.identity?.instance_key == identity.instance_key else {
                    throw MenuError.message("The response belongs to another instance. No state was adopted.")
                }
                try completion?(data)
                self.failures = 0
            } catch {
                self.failures = min(self.failures + 1, 4); self.notice = error.localizedDescription
                if arguments.first == "attention" || arguments.first == "status" {
                    self.ledger.needsBaseline = true; self.persistLedger()
                }
            }
            self.rebuildMenu()
            self.schedulePoll()
            self.openPendingActivation()
        }
    }

    private func schedulePoll() {
        timer?.invalidate()
        let delay = min(60.0, 5.0 * pow(2.0, Double(failures)))
        timer = Timer.scheduledTimer(withTimeInterval: delay, repeats: false) { [weak self] _ in
            DispatchQueue.main.async { self?.poll() }
        }
    }

    private func poll() {
        guard !busy else { return }
        perform(["status"]) { [weak self] data in
            guard let self else { return }
            self.status = try JSONDecoder().decode(ServiceStatus.self, from: data)
            self.notice = self.status?.pending_installation == true ? "Installation was interrupted. Repeat Install to reconcile it." : ""
            if let status = self.status, let pending = status.login_pending {
                self.notice = "Login startup is currently \(status.login_enabled ? "enabled" : "disabled"). Finish the interrupted \(pending.rawValue) operation."
            }
            if self.status?.observed == "running" || self.status?.observed == "draining" {
                // Enqueued only after the current request's completion has unwound.
                DispatchQueue.main.async { self.fetchAttention() }
            } else {
                self.attentionLabel = "Attention unavailable while offline"
                self.ledger.needsBaseline = true; self.persistLedger()
            }
        }
    }

    private func fetchAttention() {
        perform(["attention"]) { [weak self] data in
            guard let self else { return }
            let snapshot = try JSONDecoder().decode(AttentionSnapshot.self, from: data)
            guard snapshot.entries.count <= 100, snapshot.count >= snapshot.entries.count,
                  ["complete", "truncated", "unavailable"].contains(snapshot.state),
                  snapshot.entries.allSatisfy({ $0.attention_id.utf8.count <= 512 && ($0.episode_key == nil || $0.episode_key?.range(of: "^v1:[a-f0-9]{64}$", options: .regularExpression) != nil) }) else {
                throw MenuError.message("Attention response did not match the bounded desktop contract.")
            }
            self.attentionLabel = "\(snapshot.count) need attention" + (snapshot.state == "complete" ? "" : " (\(snapshot.state))")
            self.consume(snapshot)
        }
    }

    private func rebuildMenu() {
        let menu = NSMenu()
        menu.autoenablesItems = false
        func row(_ title: String, _ action: Selector? = nil, enabled: Bool = true) {
            let entry = NSMenuItem(title: String(title.prefix(160)), action: action, keyEquivalent: "")
            entry.target = self; entry.isEnabled = enabled && (action == nil || !busy)
            menu.addItem(entry)
        }
        row(identity?.data_dir ?? selectedRoot, nil, enabled: false)
        row(status.map { "Service: \($0.observed) · \($0.installed ? "installed" : "not installed")" } ?? "Status unavailable", nil, enabled: false)
        row(attentionLabel, nil, enabled: false)
        if !notice.isEmpty { row(notice, nil, enabled: false) }
        menu.addItem(.separator())
        row("Open dashboard", #selector(openDashboard), enabled: identity != nil)
        row("Install dormant service", #selector(install), enabled: identity != nil)
        row("Start service", #selector(start), enabled: status?.installed == true && status?.observed != "running" && status?.observed != "draining")
        row("Stop service and drain work", #selector(stop), enabled: identity != nil)
        row(status?.loginActionTitle ?? "Enable start at login", #selector(toggleLogin), enabled: status?.installed == true)
        row(notificationsEnabled ? "Disable notifications" : "Enable notifications…", #selector(toggleNotifications), enabled: identity != nil)
        row("Show diagnostics folder", #selector(showDiagnostics), enabled: identity != nil)
        row("Refresh status", #selector(refresh), enabled: identity != nil)
        row("Quit companion (leave service running)", #selector(quit))
        item.menu = menu
        item.button?.title = status?.observed == "draining" ? "LLMRelay · draining" : "LLMRelay"
    }

    @objc private func install() { perform(["install"]) { [weak self] _ in self?.notice = "Installed without starting. Choose Start when ready." } }
    @objc private func start() { perform(["start"]) { [weak self] _ in self?.notice = "Start requested. Checking the running instance…" } }
    @objc private func stop() { perform(["stop"]) { [weak self] _ in self?.notice = "Stop requested. Intent is stopped; active work may still be draining." } }
    @objc private func toggleLogin() {
        guard let status, status.installed else { return }
        perform(["login", status.nextLoginOperation.rawValue]) { [weak self] _ in self?.notice = "Login preference saved. This did not start or stop current work." }
    }
    @objc private func openDashboard() { perform(["dashboard"]) }
    @objc private func refresh() { poll() }
    @objc private func showDiagnostics() {
        guard let identity else { return }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: identity.diagnostics)])
    }
    @objc private func quit() { timer?.invalidate(); NSApplication.shared.terminate(nil) }

    @objc private func toggleNotifications() {
        guard let key = preferenceKey else { return }
        if notificationsEnabled {
            UserDefaults.standard.set(false, forKey: key); rebuildMenu(); return
        }
        center.requestAuthorization(options: [.alert, .sound]) { [weak self] allowed, error in
            DispatchQueue.main.async {
                guard let self, self.preferenceKey == key else { return }
                UserDefaults.standard.set(allowed, forKey: key)
                self.ledger.needsBaseline = true
                self.persistLedger()
                self.notice = allowed ? "Notifications enabled. Current attention establishes a quiet baseline." : "Notifications were not enabled. You can review notification settings in System Settings."
                if error != nil { self.notice = "macOS could not finish the notification request." }
                self.rebuildMenu()
            }
        }
    }

    private func ledgerURL() throws -> URL {
        guard let identity else { throw MenuError.message("No instance is selected.") }
        let home = FileManager.default.homeDirectoryForCurrentUser
        var directory = home
        for component in ["Library", "Application Support", "LLMRelayMenu", identity.instance_key] {
            directory.appendPathComponent(component, isDirectory: true)
            if !FileManager.default.fileExists(atPath: directory.path) {
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            }
            let attrs = try FileManager.default.attributesOfItem(atPath: directory.path)
            guard attrs[.type] as? FileAttributeType == .typeDirectory,
                  (attrs[.ownerAccountID] as? NSNumber)?.uint32Value == getuid() else {
                throw MenuError.message("Notification storage has unexpected ownership or is a link.")
            }
        }
        return directory.appendingPathComponent("ledger.json")
    }

    private func loadLedger() {
        do {
            let url = try ledgerURL()
            if FileManager.default.fileExists(atPath: url.path) {
                let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
                guard attrs[.type] as? FileAttributeType == .typeRegular,
                      (attrs[.ownerAccountID] as? NSNumber)?.uint32Value == getuid(),
                      ((attrs[.size] as? NSNumber)?.intValue ?? Int.max) <= 1024 * 1024 else {
                    throw MenuError.message("Notification history cannot be safely read. Delivery is disabled for this launch.")
                }
                ledger = try JSONDecoder().decode(Ledger.self, from: Data(contentsOf: url))
                guard ledger.entries.count <= 512 else { throw MenuError.message("Notification history exceeds its bound.") }
            }
            ledger.needsBaseline = true
            ledgerReady = true
        } catch { ledgerReady = false; notice = error.localizedDescription }
    }

    @discardableResult private func persistLedger() -> Bool {
        guard ledgerReady else { return false }
        do {
            let url = try ledgerURL()
            try JSONEncoder().encode(ledger).write(to: url, options: [.atomic])
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            let file = try FileHandle(forWritingTo: url)
            try file.synchronize(); try file.close()
            let fd = Darwin.open(url.deletingLastPathComponent().path, O_RDONLY)
            guard fd >= 0 else { throw MenuError.message("Notification history directory cannot be synced.") }
            defer { Darwin.close(fd) }
            guard fsync(fd) == 0 else { throw MenuError.message("Notification history could not be saved durably.") }
            return true
        } catch { ledgerReady = false; notice = "Notification history could not be saved. Delivery is disabled for this launch."; return false }
    }

    private func reconcileNotifications() {
        let notificationCenter = center
        notificationCenter.getPendingNotificationRequests { [weak self] pending in
            notificationCenter.getDeliveredNotifications { [weak self] delivered in
                let ids = Set(pending.map(\.identifier) + delivered.map { $0.request.identifier })
                DispatchQueue.main.async {
                    guard let self else { return }
                    for (key, var entry) in self.ledger.entries where entry.state == .attempted {
                        if ids.contains(entry.requestID) { entry.state = .observed; self.ledger.entries[key] = entry }
                        // Missing after a crash is ambiguous, never permission to replay.
                    }
                    self.ledgerReconciled = self.persistLedger()
                }
            }
        }
    }

    private func consume(_ snapshot: AttentionSnapshot) {
        guard ledgerReady, ledgerReconciled else { return }
        guard snapshot.state == "complete" else {
            ledger.needsBaseline = true; persistLedger(); return
        }
        guard !deliveryInFlight else { return }
        let entries = snapshot.entries.filter { $0.episode_key != nil }
        let active = Set(entries.compactMap(\.episode_key))
        if ledger.entries.count + active.filter({ ledger.entries[$0] == nil }).count > 512 {
            let retained = ledger.entries.values.sorted { $0.updated > $1.updated }
                .filter { !active.contains($0.episode) }.prefix(max(0, 512 - active.count))
            ledger.entries = ledger.entries.filter { active.contains($0.key) }
            for entry in retained { ledger.entries[entry.episode] = entry }
            ledger.needsBaseline = true
            for entry in entries { baseline(entry) }
            persistLedger()
            return // A later complete snapshot must establish the new baseline.
        }
        if ledger.needsBaseline || !notificationsEnabled {
            for entry in entries { baseline(entry) }
            ledger.needsBaseline = false; persistLedger(); return
        }
        for entry in entries {
            guard let episode = entry.episode_key else { continue }
            if let recorded = ledger.entries[episode] {
                if recorded.state == .failed && recorded.attempts == 1 { deliver(entry) ; return }
            } else { deliver(entry); return }
        }
    }

    private func baseline(_ entry: AttentionEntry) {
        guard let episode = entry.episode_key, ledger.entries[episode] == nil else { return }
        ledger.entries[episode] = Delivery(attention: entry.attention_id, episode: episode, requestID: requestID(episode), state: .baseline, attempts: 0, updated: Date())
    }

    private func deliver(_ entry: AttentionEntry) {
        guard let identity, let episode = entry.episode_key, notificationsEnabled else { return }
        let previous = ledger.entries[episode]
        let attempts = (previous?.attempts ?? 0) + 1
        guard attempts <= 2 else { return }
        let id = requestID(episode)
        ledger.entries[episode] = Delivery(attention: entry.attention_id, episode: episode, requestID: id, state: .attempted, attempts: attempts, updated: Date())
        guard persistLedger() else { return }
        deliveryInFlight = true
        let content = UNMutableNotificationContent()
        content.title = "LLMRelay needs your attention"
        content.body = "Open the dashboard to review the current item. No action has been approved."
        content.sound = .default
        content.userInfo = ["instance_key": identity.instance_key, "attention_id": entry.attention_id, "episode_key": episode]
        center.add(UNNotificationRequest(identifier: id, content: content, trigger: nil)) { [weak self] error in
            DispatchQueue.main.async {
                guard let self, self.identity?.instance_key == identity.instance_key else { return }
                self.deliveryInFlight = false
                guard var recorded = self.ledger.entries[episode], recorded.attempts == attempts else { return }
                recorded.state = error == nil ? .accepted : .failed
                recorded.updated = Date()
                self.ledger.entries[episode] = recorded
                self.persistLedger()
            }
        }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping () -> Void) {
        let info = response.notification.request.content.userInfo
        let activation = ["instance_key", "attention_id", "episode_key"].reduce(into: [String: String]()) { values, key in values[key] = info[key] as? String }
        DispatchQueue.main.async { [weak self] in self?.pendingActivation = activation; self?.openPendingActivation() }
        completionHandler()
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification, withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .sound])
    }

    private func openPendingActivation() {
        guard !busy, let activation = pendingActivation, let identity else { return }
        pendingActivation = nil
        guard activation["instance_key"] == identity.instance_key,
              let attention = activation["attention_id"], attention.utf8.count <= 512,
              let episode = activation["episode_key"], episode.range(of: "^v1:[a-f0-9]{64}$", options: .regularExpression) != nil,
              let record = ledger.entries[episode], record.attention == attention else {
            notice = "That notification belongs to another instance or unavailable attention history."; rebuildMenu(); return
        }
        perform(["dashboard", "--instance-key", identity.instance_key, "--attention-id", attention, "--episode-key", episode])
    }
}

@main
private struct MenuMain {
    @MainActor static func main() {
        let application = NSApplication.shared
        let controller = MenuController()
        application.delegate = controller
        application.setActivationPolicy(.accessory)
        application.run()
    }
}
