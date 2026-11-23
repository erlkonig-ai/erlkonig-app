import SwiftUI
import UIKit

private struct CollectionRow: Decodable, Identifiable {
    var root: String
    var name: String
    var state: String
    var selected: Bool
    var conflicted: Bool
    var readable: Bool
    var counts: DirectCounts?
    var count_error: String?
    var counts_replay_complete: Bool?
    var id: String { root }
}
private struct CollectionPage: Decodable {
    var rows: [CollectionRow] = []
    var offset = 0
    var total = 0
    var has_more = false
    var pending = 0
}

private struct DirectCounts: Decodable {
    var records: Int
    var direct_present: Int
}
private struct StorageObservation: Decodable {
    var root: String
    var resident_bytes: UInt64?
    var resident_error: String?
}
private struct VolumeCapacity: Sendable {
    var total: Int64
    var available: Int64
}

private struct ContentSegment: Decodable, Identifiable {
    var root: String
    var name: String
    var bytes: UInt64
    var id: String { root }
}
private struct ContentComposition: Decodable {
    var segments: [ContentSegment]
    var shared_bytes: UInt64
    var known_bytes: UInt64
    var known_inventories: Int
    var unknown_inventories: Int
    var error: String?

    // Physical capacity is sampled separately. Never scale or clamp a logical
    // inventory to fit it; an inconsistent sample uses the physical fallback.
    func otherUsed(in capacity: VolumeCapacity) -> UInt64? {
        guard error == nil, known_inventories > 0, capacity.total > 0,
              capacity.available >= 0, capacity.available <= capacity.total else { return nil }
        var sum = shared_bytes
        for segment in segments {
            let next = sum.addingReportingOverflow(segment.bytes)
            guard !next.overflow else { return nil }
            sum = next.partialValue
        }
        let used = UInt64(capacity.total - capacity.available)
        guard sum == known_bytes, sum <= used else { return nil }
        return used - sum
    }
}

private struct ReplayProgress: Decodable, Equatable {
    var prefix_bytes: UInt64
    var observed_bytes: UInt64
    var activity_serial: UInt64
    var fraction: Double {
        guard observed_bytes > 0 else { return 0 }
        return min(1, Double(prefix_bytes) / Double(observed_bytes))
    }
}

private struct SyncProgress: Decodable {
    var records: UInt64
    var direct_present: UInt64
    var unknown_roots: Int
    var fraction: Double? {
        guard unknown_roots == 0, records > 0, direct_present < records else { return nil }
        return Double(direct_present) / Double(records)
    }
}

private struct NetworkConnection: Decodable, Identifiable {
    var id: UInt64
    var peer: String
    var receive_bytes_per_second: Double?
    var send_bytes_per_second: Double?
    var rtt_ms: Double?
}
private struct NetworkObservation: Decodable {
    var host_epoch: UInt64
    var sampled_at: UInt64
    var age_ms: UInt64
    var connections: [NetworkConnection]
    // Presentation freshness, not a disconnect deadline. Age is measured at
    // each C-status read using the host's monotonic clock, even if it stalls.
    var isFresh: Bool { age_ms <= 5_000 }
    func current(_ metric: Double?) -> Double? { isFresh ? metric : nil }
}

private struct ReplicaStatus: Decodable {
    var node_id = ""
    var running = false
    var ready = false
    var replay_complete = false
    var replay_progress: ReplayProgress?
    var sync_progress: SyncProgress?
    var network: NetworkObservation?
    var activity_serial: UInt64 = 0
    var selected = 0
    var listening = false
    var collections = CollectionPage()
    var storage: [StorageObservation] = []
    var composition: ContentComposition?
    var action_error: String?
    var action_serial: UInt64 = 0
    var last_cleanup_error: String?
    var message = "Opening replica…"
    var error: String?
    var showsReplayProgress: Bool {
        !replay_complete || replay_progress.map { $0.prefix_bytes < $0.observed_bytes } == true
    }
    var loadingFraction: Double? {
        guard ready, error == nil else { return nil }
        return showsReplayProgress ? replay_progress?.fraction : sync_progress?.fraction
    }
    var loadingAccessibilityValue: String {
        if !showsReplayProgress {
            return "\(sync_progress?.direct_present ?? 0) of \(sync_progress?.records ?? 0) known collection records"
        }
        return "\(replay_progress?.prefix_bytes ?? 0) of \(replay_progress?.observed_bytes ?? 0) bytes"
    }
}

// Only unacknowledged user intent. The native register remains authoritative.
// A single command is outstanding; later taps coalesce by root until its ack.
private struct ChoiceQueue {
    struct Intent {
        var root: String
        var name: String
        var expected: String
        var enabled: Bool
        var page: Int?
    }
    private(set) var pending: [String: Intent] = [:]
    private var order: [String] = []
    private(set) var flight: Intent?
    private var serial: UInt64 = 0

    mutating func request(_ row: CollectionRow, enabled: Bool, page: Int?) {
        let expected = pending[row.root]?.expected ?? row.state
        pending[row.root] = Intent(root: row.root, name: row.name, expected: expected, enabled: enabled, page: page ?? pending[row.root]?.page)
        if !order.contains(row.root) { order.append(row.root) }
    }
    func desired(_ row: CollectionRow) -> Bool { pending[row.root]?.enabled ?? row.selected }
    mutating func next(serial: UInt64) -> Intent? {
        guard flight == nil else { return nil }
        while !order.isEmpty {
            let root = order.removeFirst()
            guard let intent = pending[root] else { continue }
            flight = intent; self.serial = serial
            return intent
        }
        return nil
    }
    mutating func acknowledge(serial: UInt64, error: String?) -> Intent? {
        guard let completed = flight, serial != self.serial else { return nil }
        flight = nil
        if error != nil || pending[completed.root]?.enabled == completed.enabled {
            pending.removeValue(forKey: completed.root)
            order.removeAll { $0 == completed.root }
        } else if var latest = pending[completed.root] {
            // Rebase only on our own successful durable acknowledgement. A
            // competing native write is still rejected by the next command.
            latest.expected = completed.enabled ? "selected" : "unselected"
            pending[completed.root] = latest
            if !order.contains(completed.root) { order.append(completed.root) }
        }
        return completed
    }
    mutating func failedToSend() -> Intent? {
        guard let failed = flight else { return nil }
        flight = nil; pending.removeValue(forKey: failed.root)
        order.removeAll { $0 == failed.root }
        return failed
    }
    mutating func discard() { pending.removeAll(); order.removeAll(); flight = nil }
}

@MainActor
private final class ReplicaModel: ObservableObject {
    @Published var status = ReplicaStatus()
    private var pendingActivity: UInt64 = 0
    @Published var copied = false
    @Published private var choices = ChoiceQueue()
    @Published var choiceError: String?
    @Published var began = -1.0
    @Published var ended = -1.0
    @Published var volumeCapacity: VolumeCapacity?
    @Published var volumeError: String?
    private var volumeURL: URL?
    private var volumeTask: Task<Void, Never>?
    private var handle: OpaquePointer?
    private var timer: Timer?
    private var copyTask: Task<Void, Never>?
    private var background: UIBackgroundTaskIdentifier = .invalid
    private var backgroundGeneration: UInt64 = 0
    private var foreground = true
    private var proofOnly = false
    // Disposable visible-page projection. The native register owns every choice.
    @Published private var collectionPages: [Int: CollectionPage] = [:]
    private var recentPages: [Int] = []
    private var requestedPage: Int?
    private var queuedPages: [Int] = []
    private var storageActive = false
    private var visibleStorageRoots: Set<String> = []
    private var storageRequestTask: Task<Void, Never>?
    #if SYNTHETIC_PROOF
    private var fixtureRows: [CollectionRow] = []
    private(set) var crownFixture = false
    private(set) var fixtureReducedMotion = false
    private(set) var fixtureOriginalTimeline = false
    #else
    let crownFixture = false
    let fixtureReducedMotion = false
    let fixtureOriginalTimeline = false
    #endif


    init() {
        #if SYNTHETIC_PROOF
        // Visual fixture only: return before directory/key/pile/worker setup.
        let arguments = ProcessInfo.processInfo.arguments
        if let argument = arguments.first(where: { $0.hasPrefix("--crown-fixture=") || $0.hasPrefix("--collections-fixture") }) {
            let mode = String(argument.dropFirst("--crown-fixture=".count))
            crownFixture = true
            fixtureReducedMotion = mode == "reduced"
            fixtureOriginalTimeline = arguments.contains("--crown-original-timeline")
            status.ready = true
            status.replay_complete = true
            status.running = mode != "idle" && mode != "error"
            status.selected = mode == "idle" ? 0 : 1
            status.message = "Waiting for selected collections"
            if mode == "idle" { status.message = "No collections selected" }
            if mode == "checking" { status.message = "Sync in progress" }
            if mode == "received" { status.message = "Received data saved on this device" }
            if mode == "error" { status.error = "Synthetic fixture error" }
            if argument.hasPrefix("--collections-fixture") {
                status.listening = true
                status.collections.rows = [
                    CollectionRow(root: "visual-first", name: "Atlas (synthetic)", state: "selected", selected: true, conflicted: false, readable: true),
                    CollectionRow(root: "visual-later", name: "Later grant (synthetic)", state: "unset", selected: false, conflicted: false, readable: true),
                    CollectionRow(root: "visual-conflict", name: "Conflicting choice (synthetic)", state: "conflicted:visual", selected: false, conflicted: true, readable: true),
                ]
                if argument == "--collections-fixture-many" {
                    status.collections.rows += (4...70).map { index in
                        CollectionRow(root: "visual-\(index)", name: "Collection \(index) with a longer synthetic name", state: "unset", selected: false, conflicted: false, readable: true)
                    }
                }
                for index in status.collections.rows.indices {
                    let root = status.collections.rows[index].root
                    status.collections.rows[index].counts = DirectCounts(records: root == "visual-later" ? 0 : 12,
                        direct_present: root == "visual-later" ? 0 : (root == "visual-conflict" ? 12 : 9))
                    status.collections.rows[index].counts_replay_complete = true
                }
                let measured = status.collections.rows.filter { $0.root != "visual-later" }
                let segments = measured.map { ContentSegment(root: $0.root, name: $0.name,
                    bytes: $0.root == "visual-first" ? 12_000_000_000 : ($0.root == "visual-conflict" ? 8_000_000_000 : 100_000_000)) }
                status.composition = ContentComposition(segments: segments, shared_bytes: 4_000_000_000,
                    known_bytes: segments.reduce(4_000_000_000) { $0 + $1.bytes },
                    known_inventories: segments.count, unknown_inventories: 1, error: nil)
                if argument == "--collections-fixture-prefix" {
                    status.running = false
                    status.listening = false
                    status.replay_complete = false
                    status.replay_progress = ReplayProgress(prefix_bytes: arguments.contains("--progress-fixture-small") ? 2 : 40,
                        observed_bytes: 100, activity_serial: 0)
                    status.composition = nil
                    status.collections.rows[1].state = "unknown"
                    for index in status.collections.rows.indices {
                        status.collections.rows[index].counts_replay_complete = false
                    }
                } else if argument == "--collections-fixture-network" {
                    status.replay_progress = ReplayProgress(prefix_bytes: 100, observed_bytes: 100, activity_serial: 0)
                    status.sync_progress = SyncProgress(records: 12, direct_present: 9, unknown_roots: 0)
                }
                if arguments.contains("--sync-fixture") {
                    status.message = "Sync in progress"
                    status.network = NetworkObservation(host_epoch: 1, sampled_at: 2, age_ms: 0, connections: [
                        NetworkConnection(id: 1, peer: String(repeating: "2a", count: 32),
                            receive_bytes_per_second: 1_250_000, send_bytes_per_second: 32_000, rtt_ms: 12),
                        NetworkConnection(id: 2, peer: String(repeating: "3b", count: 32),
                            receive_bytes_per_second: nil, send_bytes_per_second: nil, rtt_ms: nil)
                    ])
                }
                fixtureRows = status.collections.rows
                status.collections.total = fixtureRows.count
                status.collections.rows = Array(fixtureRows.prefix(32))
                status.collections.has_more = fixtureRows.count > 32
                collectionPages[0] = status.collections
                recentPages = [0]
            }
            if status.running { began = ProcessInfo.processInfo.systemUptime }
            return
        }
        #endif
        do {
            let support = try FileManager.default.url(for: .applicationSupportDirectory,
                in: .userDomainMask, appropriateFor: nil, create: true)
            var root = support.appendingPathComponent("ErlkonigReplica", isDirectory: true)
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true,
                attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
            // Synthetic prototype policy only; production cloud inclusion/key recovery is undecided.
            var values = URLResourceValues(); values.isExcludedFromBackup = true
            try root.setResourceValues(values)
            volumeURL = root
            #if SYNTHETIC_PROOF
            let reopening = ProcessInfo.processInfo.arguments.contains("--reopen-synthetic-proof")
            if reopening || ProcessInfo.processInfo.arguments.contains("--synthetic-proof") {
                proofOnly = true
                let fixture = root.appendingPathComponent("synthetic-proof", isDirectory: true)
                if !reopening { try FileManager.default.createDirectory(at: fixture, withIntermediateDirectories: false) }
                status.message = "Checking synthetic replica…"
                began = ProcessInfo.processInfo.systemUptime
                let path = fixture.path
                DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                    let output: String = path.withCString { pointer in
                        guard let result = (reopening ? erl_ios_reopen_proof(pointer) : erl_ios_synthetic_proof(pointer)) else { return "{}" }
                        defer { erl_ios_string_free(result) }
                        return String(cString: result)
                    }
                    Task { @MainActor [weak self] in self?.proofFinished(path: path, output: output) }
                }
                return
            }
            #endif
            open(root.path)
        } catch { status.error = error.localizedDescription; status.message = "Replica needs attention" }
    }

    deinit {
        timer?.invalidate()
        volumeTask?.cancel()
        if let handle { erl_ios_release(handle) }
    }

    private func open(_ path: String) {
        handle = path.withCString { erl_ios_open($0) }
        guard let handle else { status.error = "Could not start replica worker"; return }
        if foreground && proofOnly { pendingActivity = erl_ios_local_active(handle) }
        else { pendingActivity = erl_ios_active(handle, foreground) }
        timer = Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in self?.poll() }
        }
    }

    private func proofFinished(path: String, output: String) {
        ended = ProcessInfo.processInfo.systemUptime
        guard let data = output.data(using: .utf8),
              let report = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              report["ok"] as? Bool == true else {
            status.message = "Synthetic replica check failed"; status.error = output; return
        }
        // Keep the proof's reader offline. Reopening must not restart any endpoint.
        open(URL(fileURLWithPath: path).appendingPathComponent("reader").path)
        status.message = "Synthetic copy verified with source offline"
    }

    private func poll() {
        guard let handle, let value = erl_ios_status(handle) else { return }
        defer { erl_ios_string_free(value) }
        guard let data = String(cString: value).data(using: .utf8),
              let next = try? JSONDecoder().decode(ReplicaStatus.self, from: data) else { return }
        apply(next)
    }

    private func apply(_ incoming: ReplicaStatus) {
        var next = incoming
        // Activity acknowledgements gate commands and live network claims.
        // Local rows still describe the same retained native pile while a new
        // lifecycle request waits; their validity does not depend on its serial.
        if !foreground || next.error != nil || (!crownFixture && next.activity_serial != pendingActivity) { next.network = nil }
        if proofOnly && next.ready && next.error == nil { next.message = "Synthetic copy verified with source offline" }
        let working = foreground && next.activity_serial == pendingActivity && next.running && next.selected > 0
        var start = began, end = ended
        if erl_ios_crown_observe(ProcessInfo.processInfo.systemUptime, working, &start, &end) {
            if start != began { began = start }
            if end != ended { ended = end }
        }
        let prefixChanged = next.replay_progress?.prefix_bytes != status.replay_progress?.prefix_bytes
        if next.collections.total != status.collections.total || next.replay_complete != status.replay_complete || prefixChanged {
            collectionPages.removeAll(); recentPages.removeAll(); queuedPages.removeAll()
        }
        let offset = next.collections.offset
        collectionPages[offset] = next.collections
        recentPages.removeAll { $0 == offset }; recentPages.append(offset)
        while recentPages.count > 3 { collectionPages.removeValue(forKey: recentPages.removeFirst()) }
        if requestedPage == offset { requestedPage = nil }
        if let completed = choices.acknowledge(serial: next.action_serial, error: next.action_error) {
            if let error = next.action_error { choiceError = completed.name + ": " + error }
            if let page = completed.page, page != offset {
                // Do not leave an acknowledged earlier page's old switch visible.
                collectionPages.removeValue(forKey: page)
                if !queuedPages.contains(page) { queuedPages.insert(page, at: 0) }
            }
        }
        // Keep last completed byte observations across visibility/batch changes,
        // bounded by the same three cached pages plus currently visible roots.
        let retainedRoots = Set(collectionPages.values.flatMap { $0.rows.map(\.root) }).union(visibleStorageRoots)
        var observations = Dictionary(uniqueKeysWithValues: status.storage.filter { retainedRoots.contains($0.root) }.map { ($0.root, $0) })
        for row in next.storage where retainedRoots.contains(row.root) { observations[row.root] = row }
        next.storage = observations.values.sorted { $0.root < $1.root }
        if !next.replay_complete {
            // Byte accounting waits for complete replay; counts live in the rows.
            next.storage = next.replay_progress == nil ? [] : incoming.storage
            next.composition = nil
        }
        if next.error != nil { choices.discard() } // Fatal worker errors have no future ack.
        status = next
        requestNextPage()
        sendNextChoice()
        if !foreground && pendingActivity != 0 && next.activity_serial == pendingActivity { endBackgroundTask() }
    }

    func setForeground(_ value: Bool) {
        guard !crownFixture else { return }
        foreground = value
        status.network = nil
        if !value && background == .invalid {
            backgroundGeneration &+= 1
            let generation = backgroundGeneration
            background = UIApplication.shared.beginBackgroundTask(withName: "Save replica") { [weak self] in
                Task { @MainActor [weak self] in
                    self?.expireBackgroundTask(generation)
                }
            }
        }
        if let handle {
            if value && proofOnly { pendingActivity = erl_ios_local_active(handle) }
            else { pendingActivity = erl_ios_active(handle, value) }
        }
        if value { endBackgroundTask() }
    }
    private func expireBackgroundTask(_ generation: UInt64) {
        guard backgroundGeneration == generation else { return }
        endBackgroundTask()
    }
    private func endBackgroundTask() {
        if background != .invalid { UIApplication.shared.endBackgroundTask(background); background = .invalid }
    }
    var syncPhase: String {
        if status.error != nil { return "Needs attention" }
        if !foreground { return "Paused" }
        if !crownFixture && status.activity_serial != pendingActivity
            && status.replay_progress?.activity_serial != pendingActivity {
            return status.replay_complete ? "Resuming" : "Reading local data"
        }
        if !status.ready || !status.replay_complete { return "Reading local data" }
        if let observation = status.network {
            if !observation.isFresh { return "Waiting for update" }
            let count = observation.connections.count
            if count > 0 { return "\(count) connected " + (count == 1 ? "node" : "nodes") }
            return status.listening ? "Listening" : "No connected nodes"
        }
        if status.running && (status.listening || status.selected > 0) { return "Starting network" }
        return status.running ? "Network idle" : "Local only"
    }
    var displayedNodeID: String {
        let id = status.node_id
        return String(id.prefix(32)) + "\n" + String(id.dropFirst(32))
    }
    private var readyForCommands: Bool {
        foreground && status.replay_complete &&
            (crownFixture || (pendingActivity != 0 && status.activity_serial == pendingActivity))
    }
    var canDiscoverCollections: Bool {
        readyForCommands && !proofOnly && (crownFixture || handle != nil)
    }
    func discoverCollections() {
        guard canDiscoverCollections else { return }
        if crownFixture { status.listening = true; return }
        if let handle { erl_ios_discover(handle) }
    }
    func collectionRow(at index: Int) -> CollectionRow? {
        let offset = index / 32 * 32
        guard let rows = collectionPages[offset]?.rows, rows.indices.contains(index - offset) else { return nil }
        return rows[index - offset]
    }
    func observeCollection(at index: Int) {
        let offset = index / 32 * 32
        if offset != status.collections.offset || collectionPages[offset] == nil {
            if requestedPage != offset && !queuedPages.contains(offset) { queuedPages.append(offset) }
            requestNextPage()
        }
    }
    private func requestNextPage() {
        guard requestedPage == nil, !queuedPages.isEmpty else { return }
        let offset = queuedPages.removeFirst()
        requestedPage = offset
        #if SYNTHETIC_PROOF
        if crownFixture {
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.apply(self.fixtureStatus(at: offset))
            }
            return
        }
        #endif
        guard let handle else { requestedPage = nil; return }
        erl_ios_collections_page(handle, offset)
    }
    func desiredSelection(_ row: CollectionRow) -> Bool { choices.desired(row) }
    func choose(_ row: CollectionRow, enabled: Bool) {
        guard status.replay_complete, !proofOnly, crownFixture || handle != nil else { return }
        let page = collectionPages.first(where: { $0.value.rows.contains(where: { $0.root == row.root }) })?.key
        choices.request(row, enabled: enabled, page: page)
        choiceError = nil
        sendNextChoice()
    }
    private func sendNextChoice() {
        guard readyForCommands else { return }
        guard let intent = choices.next(serial: status.action_serial) else { return }
        #if SYNTHETIC_PROOF
        if crownFixture {
            Task { @MainActor [weak self] in
                guard let self else { return }
                var error: String?
                if let index = self.fixtureRows.firstIndex(where: { $0.root == intent.root }),
                   self.fixtureRows[index].state == intent.expected {
                    self.fixtureRows[index].selected = intent.enabled
                    self.fixtureRows[index].conflicted = false
                    self.fixtureRows[index].state = intent.enabled ? "selected" : "unselected"
                } else { error = "Collection choice changed" }
                var next = self.fixtureStatus(at: self.status.collections.offset)
                next.action_serial &+= 1
                next.action_error = error
                next.selected = self.fixtureRows.filter(\.selected).count
                self.apply(next)
            }
            return
        }
        #endif
        let sent = handle.map { handle in
            intent.root.withCString { root in
                intent.expected.withCString { erl_ios_choose(handle, root, $0, intent.enabled) }
            }
        } ?? false
        if !sent {
            if let failed = choices.failedToSend() { choiceError = failed.name + ": Could not queue this choice" }
            // Do not retry a dead channel or silently retain other unsent taps.
            choices.discard()
        }
    }
    #if SYNTHETIC_PROOF
    private func fixtureStatus(at offset: Int) -> ReplicaStatus {
        var next = status
        let start = min(offset, fixtureRows.count)
        next.collections.rows = Array(fixtureRows.dropFirst(start).prefix(32))
        next.collections.offset = start
        next.collections.total = fixtureRows.count
        next.collections.has_more = start + next.collections.rows.count < fixtureRows.count
        return next
    }
    #endif
    func setStorageActive(_ active: Bool) {
        guard storageActive != active else { return }
        storageActive = active
        if !crownFixture, !proofOnly, let handle { erl_ios_storage_active(handle, active) }
        if active { startVolumeSampling() } else { volumeTask?.cancel() }
        requestStorage()
    }
    private func startVolumeSampling() {
        // Cancellation invalidates results but cannot interrupt resourceValues.
        // Keep the task slot occupied until that read settles, even across a
        // hide/show cycle. A successor may start only after this task releases it.
        guard volumeTask == nil else { return }
        volumeTask = Task { [weak self] in
            while !Task.isCancelled, self?.storageActive == true {
                await self?.sampleVolume()
                do { try await Task.sleep(for: .seconds(5)) } catch { break }
            }
            self?.volumeTask = nil
            if self?.storageActive == true { self?.startVolumeSampling() }
        }
    }
    private func sampleVolume() async {
        #if SYNTHETIC_PROOF
        if crownFixture {
            volumeCapacity = VolumeCapacity(total: 128_000_000_000, available: 32_000_000_000)
            return
        }
        #endif
        guard let url = volumeURL else { return }
        do {
            // Fresh URL values from the app's volume, off the main actor. Ordinary
            // available capacity, not the larger purgeable/important-usage estimate.
            let sample = try await Task.detached(priority: .utility) {
                let freshURL = URL(fileURLWithPath: url.path, isDirectory: true)
                let values = try freshURL.resourceValues(forKeys: [.volumeTotalCapacityKey, .volumeAvailableCapacityKey])
                guard let total = values.volumeTotalCapacity, let available = values.volumeAvailableCapacity,
                      total > 0, available >= 0, available <= total else {
                    throw CocoaError(.fileReadUnknown)
                }
                return VolumeCapacity(total: Int64(total), available: Int64(available))
            }.value
            guard !Task.isCancelled, storageActive else { return }
            volumeCapacity = sample; volumeError = nil
        } catch {
            guard !Task.isCancelled, storageActive else { return }
            volumeCapacity = nil; volumeError = error.localizedDescription
        }
    }
    func storageVisibility(_ root: String, visible: Bool) {
        let changed = visible ? visibleStorageRoots.insert(root).inserted : visibleStorageRoots.remove(root) != nil
        guard changed else { return }
        if storageActive { requestStorage() }
    }
    private func requestStorage() {
        storageRequestTask?.cancel()
        storageRequestTask = Task { [weak self] in
            do { try await Task.sleep(for: .milliseconds(50)) } catch { return }
            guard let self else { return }
            let roots = self.storageActive ? self.visibleStorageRoots.sorted() : []
            guard roots.count <= 32 else { self.status.action_error = "Too many visible storage rows"; return }
            #if SYNTHETIC_PROOF
            if self.crownFixture {
                self.status.storage = roots.map { StorageObservation(root: $0,
                    resident_bytes: $0 == "visual-later" ? nil : ($0 == "visual-first" ? 16_000_000_000 : ($0 == "visual-conflict" ? 12_000_000_000 : 100_000_000)), resident_error: nil) }
                return
            }
            #endif
            guard let handle = self.handle, let data = try? JSONEncoder().encode(roots),
                  let json = String(data: data, encoding: .utf8) else { return }
            if !json.withCString({ erl_ios_storage_roots(handle, $0) }) {
                self.status.action_error = "Could not request storage sample"
            }
        }
    }
    func copyID() {
        guard !status.node_id.isEmpty else { return }
        UIPasteboard.general.string = status.node_id
        copied = true
        copyTask?.cancel()
        copyTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(1.5))
            if !Task.isCancelled { self?.copied = false }
        }
    }
}

private struct Crown: View {
    var began: Double
    var ended: Double
    var attention: Bool
    var fixtureReducedMotion = false
    var fixtureOriginalTimeline = false
    @Environment(\.accessibilityReduceMotion) private var reduced
    private var reduceMotion: Bool { reduced || fixtureReducedMotion }
    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30, paused: reduceMotion)) { timeline in
            // Make the scheduled timestamp an explicit Canvas input. Convert it
            // into the same Double uptime domain as began/ended, without f32.
            let frameTime = ProcessInfo.processInfo.systemUptime + timeline.date.timeIntervalSinceNow
            #if SYNTHETIC_PROOF
            canvas(at: fixtureOriginalTimeline ? nil : frameTime)
            #else
            canvas(at: frameTime)
            #endif
        }
        .accessibilityLabel("Erlkonig crown")
    }
    private func canvas(at frameTime: Double?) -> some View {
        Canvas { context, size in
                // nil reproduces the previous redraw dependency in visual fixtures only.
                let now = frameTime ?? ProcessInfo.processInfo.systemUptime
                let count = erl_ios_crown(now, began, ended, reduceMotion, attention, nil, 0)
                var points = [CrownPoint](repeating: CrownPoint(x: 0, y: 0), count: count)
                let written = points.withUnsafeMutableBufferPointer {
                    erl_ios_crown(now, began, ended, reduceMotion, attention, $0.baseAddress, $0.count)
                }
                guard written == count, count > 0 else { return }
                let scale = min(size.width / 26, size.height / 18) * 0.8
                let origin = CGPoint(x: (size.width - 26 * scale) / 2, y: (size.height - 18 * scale) / 2)
                var path = Path()
                for (index, point) in points.enumerated() {
                    let point = CGPoint(x: origin.x + point.x * scale, y: origin.y + point.y * scale)
                    if index == 0 { path.move(to: point) } else { path.addLine(to: point) }
                }
                path.closeSubpath()
                // Website RAL 9010 token (#f7f9ef); crown only.
                context.fill(path, with: .color(Color(.sRGB, red: 247.0 / 255, green: 249.0 / 255, blue: 239.0 / 255, opacity: 1)))
        }
    }
}

private struct CollectionsView: View {
    @ObservedObject var replica: ReplicaModel
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Collections").font(.title2).padding(.top)
            if replica.status.replay_complete && !replica.status.listening && replica.status.selected == 0 {
                Button("Discover collections", action: replica.discoverCollections)
                    .disabled(!replica.canDiscoverCollections)
            }
            if let error = replica.choiceError ?? replica.status.action_error ?? replica.status.error ?? replica.status.last_cleanup_error {
                Text(error).font(.footnote).foregroundStyle(.orange)
                    .contextMenu {
                        Button("Copy error", systemImage: "doc.on.doc") { UIPasteboard.general.string = error }
                    }
            }
            List {
                if replica.status.replay_complete && replica.status.collections.total == 0 {
                    Text(replica.status.listening ? "Listening for collection access" : "No available collections")
                        .foregroundStyle(.secondary)
                }
                ForEach(0..<replica.status.collections.total, id: \.self) { index in
                    Group {
                    if let row = replica.collectionRow(at: index) {
                        CollectionChoiceRow(replica: replica, row: row)
                    } else {
                        ProgressView().accessibilityLabel("Loading collection")
                    }
                    }
                    .frame(minHeight: 44)
                    .listRowBackground(Color.white.opacity(0.05))
                    .onAppear { replica.observeCollection(at: index) }
                }
            }
            .id(replica.status.collections.total)
            .listStyle(.plain).scrollContentBackground(.hidden)
            if replica.status.replay_complete && replica.status.collections.pending > 0 {
                Text("Some access metadata is unavailable").font(.caption).foregroundStyle(.secondary)
            }
            if replica.crownFixture { Text("Visual check · no replica").font(.caption) }
        }.padding(.horizontal, 20)
    }

}

private struct CollectionChoiceRow: View {
    @ObservedObject var replica: ReplicaModel
    let row: CollectionRow
    private var abbreviatedRoot: String {
        if row.root.count <= 24 { return row.root }
        return String(row.root.prefix(12)) + "…" + String(row.root.suffix(8))
    }
    private var displayedSelection: Bool {
        replica.status.replay_complete ? replica.desiredSelection(row) : row.selected
    }
    private var rowHint: String {
        let suffix = replica.status.replay_complete
            ? "Turning sync off keeps saved data." : "Observed choice in this local snapshot; changes require complete replay."
        return "Collection " + row.root + ". " + suffix
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            choiceControl(row)
            Text(abbreviatedRoot)
                .font(.system(.caption2, design: .monospaced)).foregroundStyle(.secondary)
                .accessibilityLabel("Collection " + row.root)
            if replica.status.replay_complete && !row.readable { Text("Read access unavailable").font(.caption) }
        }
        .contextMenu {
            Button("Copy collection ID", systemImage: "doc.on.doc") { UIPasteboard.general.string = row.root }
        }
        .accessibilityHint(rowHint)
        .accessibilityAction(named: Text("Copy collection ID")) { UIPasteboard.general.string = row.root }
    }
    @ViewBuilder
    private func choiceControl(_ row: CollectionRow) -> some View {
        if row.state == "unknown" {
            HStack {
                Text(row.name)
                Spacer()
                Text("—").accessibilityLabel("Selection unknown in this snapshot")
            }
        } else if row.conflicted {
            Text(row.name)
            Text("Conflicting choices · sync is off").font(.caption)
            HStack {
                Button("Sync") { replica.choose(row, enabled: true) }
                    .disabled(!replica.status.replay_complete || !row.readable)
                Button("Keep off") { replica.choose(row, enabled: false) }
                    .disabled(!replica.status.replay_complete)
            }.buttonStyle(.bordered)
        } else {
            Toggle(isOn: Binding(get: { displayedSelection }, set: { replica.choose(row, enabled: $0) })) {
                Text(row.name)
            }
            .disabled(!replica.status.replay_complete || (!row.readable && !displayedSelection))
            .accessibilityLabel("Sync " + row.name)
            .accessibilityValue(row.state == "unset" ? "Unset in this snapshot" : (displayedSelection ? "On" : "Off"))
        }
    }
}

// Exact Lissajous themes::colorhash::ral_categorical and themes::ral sRGB port.
// Hash raw collection handle bytes, as native ID widgets do, not its hex display.
private enum CollectionColorHash {
    static let palette: [UInt32] = [
        0xf9a800, // RAL 1003
        0xff9b00, // RAL 1028
        0xd4652f, // RAL 2010
        0xe75b12, // RAL 2004
        0xd47479, // RAL 3014
        0xc1121c, // RAL 3020
        0x701f29, // RAL 3004
        0x904684, // RAL 4008
        0x83639d, // RAL 4005
        0x154889, // RAL 5005
        0x2874b2, // RAL 5015
        0x07737a, // RAL 5021
        0x6a93b0, // RAL 5024
        0x48a43f, // RAL 6018
        0x81c0bb, // RAL 6027
        0x0f8558, // RAL 6032
        0x478a84, // RAL 6033
        0x468641, // RAL 6017
        0xddaf27, // RAL 1012
        0xdb6a50, // RAL 2012
        0xe1a6ad, // RAL 3015
        0xa38995, // RAL 4009
        0x3481b8, // RAL 5012
        0xb7d9b1, // RAL 6019
    ]
    static func rgb(_ bytes: [UInt8]) -> UInt32 {
        let hash = bytes.reduce(UInt64(1469598103934665603)) { ($0 ^ UInt64($1)) &* 1099511628211 }
        return palette[Int(hash % UInt64(palette.count))]
    }
    static func rootBytes(_ root: String) -> [UInt8]? {
        let text = Array(root.utf8)
        guard text.count == 64 else { return nil }
        func nibble(_ value: UInt8) -> UInt8? {
            switch value {
            case 48...57: return value - 48
            case 65...70: return value - 55
            case 97...102: return value - 87
            default: return nil
            }
        }
        var result: [UInt8] = []
        for index in stride(from: 0, to: text.count, by: 2) {
            guard let high = nibble(text[index]), let low = nibble(text[index + 1]) else { return nil }
            result.append(high * 16 + low)
        }
        return result
    }
}
private func collectionColor(_ root: String) -> Color {
    let bytes: [UInt8]
    if let decoded = CollectionColorHash.rootBytes(root) { bytes = decoded }
    else {
        #if SYNTHETIC_PROOF
        // Visual fixtures deliberately use labels instead of real collection IDs.
        guard root.hasPrefix("visual-") else { return .gray }
        bytes = Array(root.utf8)
        #else
        return .gray
        #endif
    }
    let rgb = CollectionColorHash.rgb(bytes)
    return Color(.sRGB, red: Double((rgb >> 16) & 255) / 255,
                 green: Double((rgb >> 8) & 255) / 255, blue: Double(rgb & 255) / 255, opacity: 1)
}

private struct PhoneCapacityCard: View {
    var capacity: VolumeCapacity?
    var composition: ContentComposition?
    var error: String?
    private struct Part: Identifiable {
        var id: String
        var name: String
        var bytes: UInt64
        var color: Color
    }
    private func bytes(_ value: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .file)
    }
    private var parts: [Part] {
        guard let capacity else { return [] }
        if let composition, let other = composition.otherUsed(in: capacity) {
            var result = composition.segments.filter { $0.bytes > 0 }.map {
                Part(id: $0.root, name: $0.name, bytes: $0.bytes, color: collectionColor($0.root))
            }
            if composition.shared_bytes > 0 {
                result.append(Part(id: "shared", name: "Shared", bytes: composition.shared_bytes, color: .gray))
            }
            if other > 0 { result.append(Part(id: "other", name: "Other used", bytes: other, color: .white.opacity(0.28))) }
            return result
        }
        let used = UInt64(capacity.total - capacity.available)
        return used == 0 ? [] : [Part(id: "used", name: "Used", bytes: used, color: .white.opacity(0.55))]
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .firstTextBaseline) {
                Text("iPhone").font(.headline)
                Spacer()
                Text(capacity.map { "\(bytes(UInt64($0.total - $0.available))) of \(bytes(UInt64($0.total))) used" } ?? "Unavailable")
                    .font(.caption).foregroundStyle(.secondary)
            }
            GeometryReader { geometry in
                HStack(spacing: 0) {
                    ForEach(parts) { part in
                        part.color.frame(width: capacity.map { geometry.size.width * Double(part.bytes) / Double($0.total) } ?? 0)
                    }
                    if let capacity {
                        let width = geometry.size.width * Double(capacity.available) / Double(capacity.total)
                        Color.black.opacity(0.55).frame(width: width)
                            .overlay {
                                ViewThatFits(in: .horizontal) {
                                    Text(bytes(UInt64(capacity.available))).font(.caption2).lineLimit(1).fixedSize().padding(.horizontal, 6)
                                    Color.clear
                                }
                            }
                    } else { Color.black.opacity(0.55) }
                }
                .clipShape(RoundedRectangle(cornerRadius: 7))
            }.frame(height: 28)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Phone storage")
            .accessibilityValue(capacity.map {
                "\($0.total - $0.available) bytes used, \($0.available) bytes available, \($0.total) bytes total"
            } ?? "Unavailable")

        }
        .padding(16)
        .background(Color.white.opacity(0.07), in: RoundedRectangle(cornerRadius: 18))
        .accessibilityElement(children: .contain)
        .accessibilityHint(error ?? "Known local content from complete measured inventories, not allocated disk blocks. Shared counts overlap among measured collections. Other used includes unmeasured collections, phone and system use, and storage overhead. Inventories may lag arrivals; inconsistent measurements show only used and available capacity.")
    }
}

private struct StorageView: View {
    @ObservedObject var replica: ReplicaModel
    var active: Bool
    private func bytes(_ value: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .file)
    }
    private func capacityRow(_ name: String, bytes value: UInt64, color: Color) -> some View {
        HStack(spacing: 8) {
            Circle().fill(color).frame(width: 8, height: 8).accessibilityHidden(true)
            Text(name)
            Spacer()
            Text(bytes(value)).font(.caption).foregroundStyle(.secondary)
        }
        .frame(minHeight: 44).listRowBackground(Color.white.opacity(0.05))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(name).accessibilityValue("\(value) bytes")
    }
    @ViewBuilder
    private func storageMetrics(_ row: CollectionRow) -> some View {
        let sample = replica.status.storage.first(where: { $0.root == row.root })
        HStack {
            Text(sample?.resident_bytes.map(bytes) ?? "—")
                .accessibilityLabel(sample?.resident_bytes.map { "\(bytes($0)) known resident content" } ?? "Resident size unavailable")
                .accessibilityHint(sample?.resident_error ?? "Shared content can count in other collections too")
            Spacer()
            if let counts = row.counts {
                Text("\(counts.direct_present)/\(counts.records) records")
                    .accessibilityLabel("\(counts.direct_present) of \(counts.records) locally known records have direct blobs present")
            } else {
                Text("—").accessibilityLabel("Record counts unavailable")
                    .accessibilityHint(row.count_error ?? "No count in this snapshot")
            }
        }.font(.caption).foregroundStyle(.secondary)
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Storage").font(.title2).padding(.top)
            List {
                PhoneCapacityCard(capacity: replica.volumeCapacity, composition: replica.status.composition, error: replica.volumeError)
                    .listRowInsets(EdgeInsets()).listRowBackground(Color.clear)
                Text("Local data").font(.headline)
                    .accessibilityHint("Known resident content may lag arrivals and can count in several collections. Record fractions describe direct data and metadata, not referenced files or full sync completion.")
                    .listRowBackground(Color.clear)
                if let error = replica.choiceError ?? replica.status.action_error ?? replica.status.error ?? replica.status.last_cleanup_error {
                    Text(error).font(.footnote).foregroundStyle(.orange)
                        .contextMenu {
                            Button("Copy error", systemImage: "doc.on.doc") { UIPasteboard.general.string = error }
                        }
                }
                if replica.status.replay_complete && replica.status.collections.total == 0 { Text("No available collections").foregroundStyle(.secondary) }
                ForEach(0..<replica.status.collections.total, id: \.self) { index in
                    Group {
                        if let row = replica.collectionRow(at: index) {
                            VStack(alignment: .leading, spacing: 8) {
                                HStack(spacing: 8) {
                                    Circle().fill(collectionColor(row.root)).frame(width: 8, height: 8).accessibilityHidden(true)
                                    Text(row.name)
                                }
                                storageMetrics(row)
                            }
                            .accessibilityHint("Counts describe this displayed snapshot; referenced files may still be absent. Resident bytes are the last measured inventory.")
                            .onAppear { replica.storageVisibility(row.root, visible: true) }
                            .onDisappear { replica.storageVisibility(row.root, visible: false) }
                            .id(row.root)
                        } else { ProgressView().accessibilityLabel("Loading collection") }
                    }
                    .frame(minHeight: 64).listRowBackground(Color.white.opacity(0.05))
                    .onAppear { replica.observeCollection(at: index) }
                }
                if let capacity = replica.volumeCapacity {
                    if let composition = replica.status.composition, let other = composition.otherUsed(in: capacity) {
                        if composition.shared_bytes > 0 {
                            capacityRow("Shared", bytes: composition.shared_bytes, color: .gray)
                        }
                        if other > 0 { capacityRow("Other used", bytes: other, color: .white.opacity(0.28)) }
                    } else if capacity.total > capacity.available {
                        capacityRow("Used", bytes: UInt64(capacity.total - capacity.available), color: .white.opacity(0.55))
                    }
                }
            }
            .id(replica.status.collections.total)
            .listStyle(.plain).scrollContentBackground(.hidden)
            if replica.crownFixture { Text("Visual check · no replica").font(.caption) }
        }
        .padding(.horizontal, 20)
        .onAppear { replica.setStorageActive(active) }
        .onChange(of: active) { _, value in replica.setStorageActive(value) }
        .onDisappear { replica.setStorageActive(false) }
    }
}

private struct SyncView: View {
    @ObservedObject var replica: ReplicaModel
    private func rate(_ value: Double?) -> String {
        guard let value, value.isFinite, value >= 0 else { return "—" }
        if value >= 1_000_000 { return String(format: "%.1f MB/s", value / 1_000_000) }
        if value >= 1_000 { return String(format: "%.1f kB/s", value / 1_000) }
        return String(format: "%.0f B/s", value)
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Sync").font(.title2).padding(.top)
            Text(replica.syncPhase).font(.caption).foregroundStyle(.secondary)
            if let observation = replica.status.network {
                List(observation.connections) { connection in
                    VStack(alignment: .leading, spacing: 8) {
                        Button {
                            UIPasteboard.general.string = connection.peer
                        } label: {
                            Text(String(connection.peer.prefix(12)) + "…" + String(connection.peer.suffix(8)))
                                .font(.system(.caption, design: .monospaced))
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Copy node ID " + connection.peer)
                        HStack {
                            Text("↓ " + rate(observation.current(connection.receive_bytes_per_second)))
                                .accessibilityLabel("Receive " + rate(observation.current(connection.receive_bytes_per_second)))
                            Spacer()
                            Text("↑ " + rate(observation.current(connection.send_bytes_per_second)))
                                .accessibilityLabel("Send " + rate(observation.current(connection.send_bytes_per_second)))
                        }.font(.caption)
                        .accessibilityHint("Network traffic includes protocol overhead and retransmissions")
                        if let rtt = observation.current(connection.rtt_ms) {
                            Text(String(format: "RTT ≈ %.0f ms", rtt))
                                .font(.caption).foregroundStyle(.secondary)
                                .accessibilityLabel(String(format: "Estimated network round trip %.0f milliseconds", rtt))
                        }
                    }.padding(.vertical, 4)
                }.listStyle(.plain).scrollContentBackground(.hidden)
            }
            Spacer(minLength: 0)
        }.padding(.horizontal)
    }
}

private struct BottomProgressStrip: View {
    let fraction: Double
    let syntheticFixture: Bool

    var body: some View {
        if #available(iOS 26.0, *) {
            CornerProgressStrip(fraction: fraction, syntheticFixture: syntheticFixture)
                .ignoresSafeArea()
        } else {
            GeometryReader { geometry in
                Rectangle().fill(Color.white)
                    .frame(width: geometry.size.width * CGFloat(fraction), height: 8)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomLeading)
            }
        }
    }
}

@available(iOS 26.0, *)
private struct CornerProgressStrip: UIViewRepresentable {
    let fraction: Double
    let syntheticFixture: Bool

    func makeUIView(context: Context) -> CornerProgressStripView {
        CornerProgressStripView()
    }

    func updateUIView(_ view: CornerProgressStripView, context: Context) {
        view.fraction = fraction
        #if SYNTHETIC_PROOF
        view.reportsGeometry = syntheticFixture && ProcessInfo.processInfo.arguments.contains("--progress-geometry-proof")
        #endif
        view.setNeedsLayout()
    }
}

@available(iOS 26.0, *)
private final class CornerProgressStripView: UIView {
    var fraction: Double = 0
    private let strip = CALayer()
    #if SYNTHETIC_PROOF
    var reportsGeometry = false
    private var lastObservation: String?
    #endif

    init() {
        super.init(frame: .zero)
        backgroundColor = .clear
        isUserInteractionEnabled = false
        cornerConfiguration = .corners(radius: .containerConcentric())
        strip.backgroundColor = UIColor.white.cgColor
        strip.isHidden = true
        layer.addSublayer(strip)
    }

    required init?(coder: NSCoder) { nil }

    override func layoutSubviews() {
        super.layoutSubviews()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        defer { CATransaction.commit() }
        // Wait for an attached full-window observation; never invent an initial radius.
        guard let window, convert(bounds, to: window) == window.bounds,
              bounds.width > 0, bounds.height >= 8, fraction.isFinite else {
            strip.isHidden = true
            return
        }
        let left = effectiveRadius(corner: .bottomLeft)
        let right = effectiveRadius(corner: .bottomRight)
        guard left.isFinite, right.isFinite, left >= 0, right >= 0 else {
            strip.isHidden = true
            return
        }
        // The system-resolved radii define a conservative flat span, not a pixel mask.
        let span = max(0, bounds.width - left - right)
        strip.frame = CGRect(x: bounds.minX, y: bounds.maxY - 8,
                             width: left + span * CGFloat(min(1, max(0, fraction))), height: 8)
        strip.isHidden = false
        #if SYNTHETIC_PROOF
        if reportsGeometry {
            let frame = convert(bounds, to: window)
            let values: [String: Any] = [
                "frame_in_window": [frame.minX, frame.minY, frame.width, frame.height],
                "window_bounds": [window.bounds.minX, window.bounds.minY, window.bounds.width, window.bounds.height],
                "bottom_left_radius": left, "bottom_right_radius": right,
                "track_width": span, "fraction": fraction,
                "strip_frame": [strip.frame.minX, strip.frame.minY, strip.frame.width, strip.frame.height],
                "system_version": UIDevice.current.systemVersion,
            ]
            if let bytes = try? JSONSerialization.data(withJSONObject: values, options: [.sortedKeys]),
               let observation = String(data: bytes, encoding: .utf8), observation != lastObservation {
                lastObservation = observation
                FileHandle.standardError.write(Data(("ERLKONIG_PROGRESS_GEOMETRY " + observation + "\n").utf8))
            }
        }
        #endif
    }
}

@main
struct ErlkonigPhoneApp: App {
    @StateObject private var replica = ReplicaModel()
    @Environment(\.scenePhase) private var phase
    @State private var page = 0
    var body: some Scene {
        WindowGroup {
            TabView(selection: $page) {
            VStack(spacing: 16) {
                Crown(began: replica.began, ended: replica.ended, attention: replica.status.error != nil,
                    fixtureReducedMotion: replica.fixtureReducedMotion,
                    fixtureOriginalTimeline: replica.fixtureOriginalTimeline)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                if let error = replica.status.error {
                    Text(error).font(.footnote).foregroundStyle(.orange)
                        .contextMenu {
                            Button("Copy error", systemImage: "doc.on.doc") { UIPasteboard.general.string = error }
                        }
                        .multilineTextAlignment(.center).padding(.horizontal)
                }
                if replica.crownFixture {
                    Text("Visual check · no replica")
                        .font(.caption).foregroundStyle(.white.opacity(0.65)).padding(.bottom, 12)
                } else {
                Button(action: replica.copyID) {
                    Text(replica.copied ? "Copied" : (replica.status.node_id.isEmpty ? "Preparing node ID…" : replica.displayedNodeID))
                        .font(.system(.caption, design: .monospaced)).multilineTextAlignment(.center)
                        .foregroundStyle(.white.opacity(0.75))
                        .lineLimit(2).frame(maxWidth: .infinity, minHeight: 44)
                }
                .disabled(replica.status.node_id.isEmpty)
                .accessibilityLabel("Copy node ID")
                .padding(.horizontal, 24).padding(.bottom, 12)
                }
            }
            .tag(0)
            CollectionsView(replica: replica).tag(1)
            StorageView(replica: replica, active: page == 2 && phase == .active).tag(2)
            SyncView(replica: replica).tag(3)
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
            .accessibilityAction(named: Text("Show crown")) { page = 0 }
            .accessibilityAction(named: Text("Show collections")) { page = 1 }
            .accessibilityAction(named: Text("Show storage")) { page = 2 }
            .accessibilityAction(named: Text("Show sync")) { page = 3 }
            .accessibilityScrollAction { edge in
                if edge == .trailing { page = min(3, page + 1) }
                if edge == .leading { page = max(0, page - 1) }
            }
            .overlay(alignment: .bottomLeading) {
                if let fraction = replica.status.loadingFraction {
                    BottomProgressStrip(fraction: fraction, syntheticFixture: replica.crownFixture)
                    .allowsHitTesting(false)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(replica.status.showsReplayProgress ? "Local pile replay" : "Collection record synchronization")
                    .accessibilityValue(replica.status.loadingAccessibilityValue)
                }
            }
            .background(Color.black.ignoresSafeArea())
            .foregroundStyle(.white)
            .preferredColorScheme(.dark)
            .onChange(of: phase) { _, next in
                if next == .background { replica.setForeground(false) }
                if next == .active { replica.setForeground(true) }
            }
        }
    }
}
