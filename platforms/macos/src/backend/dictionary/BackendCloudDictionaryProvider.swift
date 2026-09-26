import Foundation

protocol DesktopCloudDictionaryAPI {
  func dictionaryCatalog(_ kind: BackendAccountClient.DictionaryKind, code: String, offset: Int, scheme: String, profile: String, token: String) async throws -> BackendAccountClient.DictionaryCatalog
  func editCatalog(_ entry: BackendAccountClient.CatalogEntry, revision: Int64, replacement: BackendAccountClient.DictionaryValue?, token: String) async throws -> BackendAccountClient.DictionaryChange
  func personalCandidates(_ query: BackendAccountClient.CandidateQuery, token: String) async throws -> BackendAccountClient.PersonalCandidates
  func rankCandidate(_ candidate: BackendAccountClient.PersonalCandidate, query: BackendAccountClient.CandidateQuery, revision: Int64, mode: BackendAccountClient.RankingMode, step: Int, trigger: Int, forceTop: Bool, token: String) async throws -> BackendAccountClient.RankingResult
  func removeCandidate(_ candidate: BackendAccountClient.PersonalCandidate, query: BackendAccountClient.CandidateQuery, revision: Int64, token: String) async throws -> BackendAccountClient.DictionaryChange
  func fixedPositions(context: String, offset: Int, token: String) async throws -> BackendAccountClient.FixedPositions
  func setFixedPosition(context: String, code: String, word: String, position: Int?, revision: Int64, token: String) async throws -> BackendAccountClient.DictionaryRevision
  func dictionary(_ kind: BackendAccountClient.DictionaryKind, search: String, offset: Int, token: String) async throws -> BackendAccountClient.DictionaryPage
  func addDictionary(_ kind: BackendAccountClient.DictionaryKind, value: BackendAccountClient.DictionaryValue, token: String) async throws -> BackendAccountClient.DictionaryChange
  func updateDictionary(_ entry: BackendAccountClient.DictionaryEntry, value: BackendAccountClient.DictionaryValue, token: String) async throws -> BackendAccountClient.DictionaryChange
  func deleteDictionary(_ entry: BackendAccountClient.DictionaryEntry, token: String) async throws -> BackendAccountClient.DictionaryChange
  func importDictionary(_ kind: BackendAccountClient.DictionaryKind, text: String, format: BackendAccountClient.DictionaryFileFormat, token: String) async throws -> BackendAccountClient.DictionaryImportResult
  func exportDictionary(_ kind: BackendAccountClient.DictionaryKind, format: BackendAccountClient.DictionaryFileFormat, token: String) async throws -> URL
}
extension BackendAccountClient: DesktopCloudDictionaryAPI {}

/// Account credentials stay in the native actor. Only dictionary values and a
/// private, bounded export descriptor cross the authenticated desktop channel.
@MainActor @objc(MSIMEBackendCloudDictionaryProvider)
final class BackendCloudDictionaryProvider: NSObject {
  private let client: any DesktopCloudDictionaryAPI
  private let credentials: () async throws -> String
  private lazy var snapshots: BackendDesktopSnapshots? = {
    guard let client = client as? any DesktopSnapshotAPI else { return nil }
    return BackendDesktopSnapshots(client: client, credentials: credentials)
  }()
  private var exported: URL?
  init(client: any DesktopCloudDictionaryAPI, credentials: @escaping () async throws -> String) {
    self.client = client; self.credentials = credentials
  }
  deinit { if let exported { try? FileManager.default.removeItem(at: exported.deletingLastPathComponent()) } }

  @objc static func prepare(completion: @escaping (BackendCloudDictionaryProvider?) -> Void) {
    Task {
      do {
        guard let user = try await BackendAccountSession.shared.user() else { completion(nil); return }
        completion(BackendCloudDictionaryProvider(client: BackendAccountClient(), credentials: {
          try await BackendAccountSession.shared.credentials(matchingUserID: user.id).token
        }))
      } catch { completion(nil) }
    }
  }

  private static func number(_ value: Any?, minimum: Int64 = 0, maximum: Int64 = 9_007_199_254_740_991) throws -> Int64 {
    guard let value = value as? NSNumber, CFGetTypeID(value) != CFBooleanGetTypeID(),
          value.doubleValue.isFinite, value.doubleValue.rounded() == value.doubleValue,
          value.doubleValue >= Double(minimum), value.doubleValue <= Double(maximum) else { throw BackendAccountClient.Failure(status: 400) }
    return value.int64Value
  }
  private static func text(_ value: Any?, limit: Int, empty: Bool = false, multiline: Bool = false) throws -> String {
    guard let value = value as? String, (empty || !value.isEmpty), value.utf8.count <= limit,
          !value.unicodeScalars.contains(where: { $0.properties.generalCategory == .control && !(multiline && [9,10,13].contains($0.value)) }) else { throw BackendAccountClient.Failure(status: 400) }
    return value
  }
  private static func identity(_ value: Any?) throws -> String {
    let value = try text(value, limit: 64)
    guard value.utf8.count == 64, value.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else { throw BackendAccountClient.Failure(status: 400) }
    return value
  }
  private static func dictionaryValue(_ request: NSDictionary, kind: BackendAccountClient.DictionaryKind) throws -> BackendAccountClient.DictionaryValue {
    let maximum = kind == .wubi ? 4 : kind == .quick ? 32 : kind == .english ? 64 : 256
    let code = try text(request["code"], limit: maximum)
    let word = try text(request["word"], limit: 1024)
    guard code.utf8.allSatisfy({ byte in
      switch kind {
      case .pinyin: return (97...122).contains(byte) || byte == 39 || byte == 32
      case .wubi: return (97...122).contains(byte)
      case .quick: return (97...122).contains(byte) || (48...57).contains(byte)
      case .english: return (97...122).contains(byte) || (65...90).contains(byte)
      }
    }), kind != .quick || word.utf16.count <= 199 else { throw BackendAccountClient.Failure(status: 400) }
    return .init(code: code, word: word, weight: try number(request["weight"]))
  }
  private enum Action {
    case catalog(BackendAccountClient.DictionaryKind, String, Int, String, String)
    case editCatalog(BackendAccountClient.CatalogEntry, Int64, BackendAccountClient.DictionaryValue?)
    case candidates(BackendAccountClient.CandidateQuery)
    case rank(BackendAccountClient.PersonalCandidate, BackendAccountClient.CandidateQuery, Int64, BackendAccountClient.RankingMode, Int, Int, Bool)
    case removeCandidate(BackendAccountClient.PersonalCandidate, BackendAccountClient.CandidateQuery, Int64)
    case positions(String, Int)
    case setPosition(String, String, String, Int?, Int64)
    case list(BackendAccountClient.DictionaryKind, Int, String)
    case add(BackendAccountClient.DictionaryKind, BackendAccountClient.DictionaryValue)
    case update(BackendAccountClient.DictionaryEntry, BackendAccountClient.DictionaryValue)
    case delete(BackendAccountClient.DictionaryEntry)
    case `import`(BackendAccountClient.DictionaryKind, BackendAccountClient.DictionaryFileFormat, String)
    case export(BackendAccountClient.DictionaryKind, BackendAccountClient.DictionaryFileFormat)
    @MainActor init(_ request: NSDictionary) throws {
      let operation = request["operation"] as? String
      if operation == "fixed_positions" {
        self = .positions(try text(request["context"], limit: 1024, empty: true), Int(try number(request["offset"], maximum: 1_000_000))); return
      }
      if operation == "set_fixed_position" {
        guard request["position"] != nil else { throw BackendAccountClient.Failure(status: 400) }
        let position = request["position"] is NSNull ? nil : Int(try number(request["position"], minimum: 1, maximum: 5))
        self = .setPosition(try text(request["context"], limit: 1024), try text(request["code"], limit: 256), try text(request["word"], limit: 1024), position, try number(request["revision"])); return
      }
      if ["candidates", "rank", "remove_candidate"].contains(operation ?? "") {
        guard let kind = request["kind"] as? String, ["pinyin", "jianpin", "wubi", "quick", "english"].contains(kind) else { throw BackendAccountClient.Failure(status: 400) }
        let query = BackendAccountClient.CandidateQuery(text: try text(request["text"], limit: 256), kind: kind,
          scheme: try text(request["scheme"], limit: 64), profile: try text(request["profile"], limit: 64), limit: Int(try number(request["limit"], minimum: 1, maximum: 100)))
        guard ["pinyin", "shuangpin"].contains(query.scheme), ["xiaohe", "ziranma", "microsoft", "shoudao"].contains(query.profile) else { throw BackendAccountClient.Failure(status: 400) }
        if operation == "candidates" { self = .candidates(query); return }
        guard kind != "quick" else { throw BackendAccountClient.Failure(status: 400) }
        // The shared UI sends canonical pinyin as code for mutation, not the display abbreviation.
        let candidate = BackendAccountClient.PersonalCandidate(code: try text(request["code"], limit: 256), word: try text(request["word"], limit: 1024), weight: 0, canonical_pinyin: nil)
        let revision = try number(request["revision"])
        if operation == "remove_candidate" { self = .removeCandidate(candidate, query, revision); return }
        guard let name = request["mode"] as? String, let mode = BackendAccountClient.RankingMode(rawValue: name),
              let force = request["force_top"] as? NSNumber, CFGetTypeID(force) == CFBooleanGetTypeID() else { throw BackendAccountClient.Failure(status: 400) }
        self = .rank(candidate, query, revision, mode, Int(try number(request["linear_step"], minimum: 1, maximum: 100)), Int(try number(request["trigger_count"], minimum: 1, maximum: 10)), force.boolValue); return
      }
      guard let name = request["kind"] as? String, let kind = BackendAccountClient.DictionaryKind(rawValue: name) else { throw BackendAccountClient.Failure(status: 400) }
      switch request["operation"] as? String {
      case "catalog": self = .catalog(kind, try text(request["code"], limit: 256, empty: kind == .quick), Int(try number(request["offset"], maximum: 1_000_000)), try text(request["scheme"], limit: 64), try text(request["profile"], limit: 64))
      case "edit_catalog":
        guard request["replacement"] is NSNull || request["replacement"] is NSDictionary else { throw BackendAccountClient.Failure(status: 400) }
        let previous = BackendAccountClient.CatalogEntry(kind: kind, code: try text(request["code"], limit: 256), word: try text(request["word"], limit: 1024), weight: 0)
        self = .editCatalog(previous, try number(request["revision"]), try (request["replacement"] as? NSDictionary).map { try dictionaryValue($0, kind: kind) })
      case "list": self = .list(kind, Int(try number(request["offset"], maximum: 1_000_000)), try text(request["search"], limit: 1024, empty: true))
      case "add": self = .add(kind, try dictionaryValue(request, kind: kind))
      case "update", "delete":
        let entry = BackendAccountClient.DictionaryEntry(id: try identity(request["id"]), kind: kind, code: "", word: "", weight: 0, revision: try number(request["revision"], minimum: 1))
        self = request["operation"] as? String == "delete" ? .delete(entry) : .update(entry, try dictionaryValue(request, kind: kind))
      case "import", "export":
        guard let name = request["format"] as? String, let format = BackendAccountClient.DictionaryFileFormat(rawValue: name),
              format != .hans || (kind == .pinyin && request["operation"] as? String == "import") else { throw BackendAccountClient.Failure(status: 400) }
        self = request["operation"] as? String == "export" ? .export(kind, format) : .import(kind, format, try text(request["text"], limit: 65536, multiline: true))
      default: throw BackendAccountClient.Failure(status: 400)
      }
    }
  }
  private func entry(_ value: BackendAccountClient.DictionaryEntry) throws -> [String: Any] {
    let id = try Self.identity(value.id)
    let code = try Self.text(value.code, limit: 256), word = try Self.text(value.word, limit: 1024)
    guard value.weight >= 0, value.revision > 0 else { throw BackendAccountClient.Failure(status: 0) }
    return ["id":id, "kind":value.kind.rawValue, "code":code, "word":word, "weight":value.weight, "revision":value.revision]
  }
  private func change(_ value: BackendAccountClient.DictionaryChange) throws -> [String: Any] {
    guard value.revision > 0 else { throw BackendAccountClient.Failure(status: 0) }
    return ["revision":value.revision, "previous":try value.previous.map(entry) as Any? ?? NSNull(), "replacement":try value.replacement.map(entry) as Any? ?? NSNull()]
  }
  func execute(_ request: NSDictionary) async throws -> [String: Any] {
    if let operation = request["operation"] as? String, operation.hasPrefix("snapshot_"), let snapshots {
      return try await snapshots.execute(request)
    }
    let action = try Action(request)
    let token = try await credentials()
    try Task.checkCancellation()
    var pendingExport: URL?
    defer { if let pendingExport { try? FileManager.default.removeItem(at: pendingExport.deletingLastPathComponent()) } }
    let result: [String: Any]
    switch action {
    case .catalog(let kind, let code, let offset, let scheme, let profile):
      let page = try await client.dictionaryCatalog(kind, code: code, offset: offset, scheme: scheme, profile: profile, token: token)
      guard page.entries.count <= 100, page.offset == offset, page.entries.allSatisfy({ $0.kind == kind }) else { throw BackendAccountClient.Failure(status: 0) }
      let entries = try page.entries.map { value -> [String: Any] in
        ["kind":value.kind.rawValue, "code":try Self.text(value.code, limit: 256), "word":try Self.text(value.word, limit: 1024), "weight":try Self.number(value.weight)]
      }
      result = ["catalog_entries":entries, "offset":offset, "has_more":page.has_more, "revision":try Self.number(page.revision), "normalized":try Self.text(page.normalized, limit: 256, empty: true)]
    case .editCatalog(let entry, let revision, let replacement):
      result = try change(await client.editCatalog(entry, revision: revision, replacement: replacement, token: token))
    case .candidates(let query):
      let page = try await client.personalCandidates(query, token: token)
      guard page.candidates.count <= query.limit else { throw BackendAccountClient.Failure(status: 0) }
      let candidates = try page.candidates.map { value -> [String: Any] in
        ["code":try Self.text(value.code, limit: 256), "word":try Self.text(value.word, limit: 1024), "weight":try Self.number(value.weight), "canonical_pinyin":try value.canonical_pinyin.map { try Self.text($0, limit: 256, empty: true) } as Any? ?? NSNull()]
      }
      result = ["candidates":candidates, "context":try Self.text(page.context, limit: 1024, empty: true), "revision":try Self.number(page.revision)]
    case .rank(let candidate, let query, let revision, let mode, let step, let trigger, let force):
      let ranked = try await client.rankCandidate(candidate, query: query, revision: revision, mode: mode, step: step, trigger: trigger, forceTop: force, token: token)
      result = ["revision":try Self.number(ranked.revision), "changed":ranked.changed, "selection_count":try Self.number(ranked.selection.count)]
    case .removeCandidate(let candidate, let query, let revision):
      result = try change(await client.removeCandidate(candidate, query: query, revision: revision, token: token))
    case .positions(let context, let offset):
      let page = try await client.fixedPositions(context: context, offset: offset, token: token)
      guard page.positions.count <= 100, page.offset == offset, context.isEmpty || page.positions.allSatisfy({ $0.context == context }) else { throw BackendAccountClient.Failure(status: 0) }
      let positions = try page.positions.map { value -> [String: Any] in
        ["context":try Self.text(value.context, limit: 1024), "code":try Self.text(value.code, limit: 256), "word":try Self.text(value.word, limit: 1024), "position":try Self.number(value.position, minimum: 1, maximum: 5)]
      }
      result = ["positions":positions, "offset":offset, "has_more":page.has_more]
    case .setPosition(let context, let code, let word, let position, let revision):
      let changed = try await client.setFixedPosition(context: context, code: code, word: word, position: position, revision: revision, token: token)
      result = ["revision":try Self.number(changed.revision)]
    case .list(let kind, let offset, let search):
      let page = try await client.dictionary(kind, search: search, offset: offset, token: token)
      guard page.entries.count <= 100, page.offset == offset, page.entries.allSatisfy({ $0.kind == kind }) else { throw BackendAccountClient.Failure(status: 0) }
      result = ["entries":try page.entries.map(entry), "offset":page.offset, "has_more":page.has_more]
    case .add(let kind, let value): result = try change(await client.addDictionary(kind, value: value, token: token))
    case .update(let entry, let value): result = try change(await client.updateDictionary(entry, value: value, token: token))
    case .delete(let entry): result = try change(await client.deleteDictionary(entry, token: token))
    case .import(let kind, let format, let text):
      let imported = try await client.importDictionary(kind, text: text, format: format, token: token)
      result = ["imported":imported.imported, "revision":imported.revision]
    case .export(let kind, let format):
      let file = try await client.exportDictionary(kind, format: format, token: token)
      guard file.isFileURL, file.lastPathComponent == "dictionary-" + kind.rawValue + ".tsv",
            file.deletingLastPathComponent().lastPathComponent.hasPrefix("msime-export-") else { throw BackendAccountClient.Failure(status: 0) }
      pendingExport = file
      let attributes = try FileManager.default.attributesOfItem(atPath: file.path)
      guard let bytes = attributes[.size] as? NSNumber, bytes.int64Value <= 384 * 1024 * 1024 else { throw BackendAccountClient.Failure(status: 0) }
      result = ["export_file":["path":file.path, "bytes":bytes], "filename":file.lastPathComponent]
    }
    _ = try await credentials()
    try Task.checkCancellation()
    if let file = pendingExport {
      if let exported { try? FileManager.default.removeItem(at: exported.deletingLastPathComponent()) }
      exported = file; pendingExport = nil
    }
    return result
  }
  @objc func request(_ request: NSDictionary, completion: @escaping (NSDictionary) -> Void) -> Progress {
    let progress = Progress(totalUnitCount: 1)
    let task = Task {
      do {
        // Progress.cancel() marks the progress synchronously but invokes its
        // cancellation handler asynchronously. Check that flag before entering
        // execute so a request cancelled before this task is scheduled never
        // fetches credentials or starts network I/O.
        guard !progress.isCancelled else { throw CancellationError() }
        completion(["ok":true, "value":try await execute(request)])
      }
      catch let failure as BackendAccountClient.Failure { completion(["ok":false, "error":failure.status == 409 ? "conflict" : "unavailable"]) }
      catch { completion(["ok":false, "error":"unavailable"]) }
      progress.completedUnitCount = 1; progress.cancellationHandler = nil
    }
    progress.cancellationHandler = { task.cancel() }
    return progress
  }
}
