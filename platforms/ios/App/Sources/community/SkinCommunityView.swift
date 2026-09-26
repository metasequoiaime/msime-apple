import SwiftUI

struct SkinCommunityView: View {
  var onlyMine = false
  var embedded = false
  private var visibleSkins: [CommunitySkin] { onlyMine ? skins.filter(\.owned) : skins }
  @State private var skins: [CommunitySkin] = []
  @State private var more = false
  @State private var busy = false
  @State private var signedIn = false
  @State private var message: String?
  @State private var search = ""
  @State private var showPublish = false
  @State private var showAccount = false
  @State private var requestID = UUID()
  private let api = SkinCommunityAPI.shared

  private var gallery: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: 16) {
        CommunitySearchField(text: $search, placeholder: "搜索皮肤设计") { run { try await load() } }
        VStack(alignment: .leading, spacing: 4) {
          Text(onlyMine ? "你的公开设计" : "换个心情，从键盘开始").font(.system(size: 20, weight: .bold))
          Text("发现创作者的配色与巧思，找到你的那一款")
            .font(.caption).foregroundStyle(.secondary)
        }.padding(.vertical, 2)
        if visibleSkins.isEmpty && !busy {
          Text(onlyMine ? (more ? "当前页没有你的作品，继续加载查看更多。" : "还没有已发布的作品，分享你的第一款设计吧。") : "暂时没有皮肤，发布你的第一款设计吧。")
            .foregroundStyle(.secondary).frame(maxWidth: .infinity).padding(.vertical, 40)
        }
        LazyVGrid(columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)], spacing: 14) {
          ForEach(visibleSkins) { skin in
            NavigationLink { CommunitySkinDetail(initial: skin) } label: {
              CommunitySkinCard(skin: skin)
            }.buttonStyle(.plain).accessibilityIdentifier("communitySkinCard-\(skin.id)")
          }
        }
        if more { Button("加载更多") { run { try await load(append: true) } }.disabled(busy).frame(maxWidth: .infinity) }
        if busy { ProgressView().frame(maxWidth: .infinity) }
      }.padding(16)
    }
    .background(Color(uiColor: .systemGroupedBackground))
  }
  var body: some View {
    gallery
    .toolbar {
      ToolbarItem(placement: .navigationBarTrailing) { if !embedded {
        Button { if signedIn { showPublish = true } else { showAccount = true } } label: {
          Label("发布", systemImage: "plus")
        }.accessibilityLabel("发布我的设计").accessibilityIdentifier("publishCommunitySkin")
      }
      }
    }
    .navigationTitle(onlyMine ? "我发布的皮肤" : (embedded ? "社区" : "皮肤社区"))
    .navigationBarTitleDisplayMode(.inline)
    .refreshable { do { try await load() } catch { message = error.localizedDescription } }
    .task {
      signedIn = (try? await api.signedIn()) ?? false
      run { try await load() }
    }
    .onChange(of: signedIn) { _ in
      Task { do { try await load() } catch { message = error.localizedDescription } }
    }
    .sheet(isPresented: $showPublish) { CommunityPublishView { run { try await load() } } }
    .sheet(isPresented: $showAccount, onDismiss: {
      Task {
        signedIn = (try? await api.signedIn()) ?? false
        if signedIn { showPublish = true }
      }
    }) { AccountLoginSheet() }
    .alert("皮肤社区", isPresented: Binding(get: { message != nil }, set: { if !$0 { message = nil } })) {
      Button("好", role: .cancel) {}
    } message: { Text(message ?? "") }
  }
  @MainActor private func load(append: Bool = false) async throws {
    let id = UUID()
    requestID = id
    let page: CommunityPage
    do { page = try await api.list(offset: append ? skins.count : 0, search: search) }
    catch {
      guard requestID == id else { return }
      throw error
    }
    guard requestID == id else { return }
    if append { let ids = Set(skins.map(\.id)); skins += page.skins.filter { !ids.contains($0.id) } }
    else { skins = page.skins }
    more = page.has_more
  }
  private func run(_ action: @escaping @MainActor () async throws -> Void) {
    guard !busy else { return }; busy = true
    Task { defer { busy = false }; do { try await action() } catch { message = error.localizedDescription } }
  }
}

private struct CommunitySkinCard: View {
  let skin: CommunitySkin
  var body: some View {
    VStack(alignment: .leading, spacing: 9) {
      CommunityDesignPreview(design: skin.design)
      Text(skin.name).font(.system(size: 15, weight: .semibold)).lineLimit(1)
      CommunityAuthorLabel(name: skin.owned ? "我的作品" : skin.author)
      HStack(spacing: 3) {
        Label("\(skin.downloads)", systemImage: "arrow.down.to.line")
        Spacer(minLength: 2)
        Label(skin.rating_count == 0 ? "暂无评分" : String(format: "%.1f", skin.rating_average), systemImage: "star")
      }.font(.system(size: 10)).foregroundStyle(.secondary)
    }.padding(10).background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 19))
      .overlay(RoundedRectangle(cornerRadius: 19).strokeBorder(Color.primary.opacity(0.035), lineWidth: 1))
  }

}

struct CommunitySkinDetail: View {
  let initial: CommunitySkin
  @State private var updated: CommunitySkin?
  @State private var busy = false
  @State private var message: String?
  @State private var confirmsRemoval = false
  @State private var trial: KeyboardSkinTrial?
  @Environment(\.dismiss) private var dismiss
  private var skin: CommunitySkin { updated ?? initial }
  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: 16) {
        CommunityDesignPreview(design: skin.design)
        Text(skin.name).font(.title2.bold())
        Text(skin.author).foregroundStyle(.secondary)
        Text(skin.description)
        Text("\(skin.downloads) 人下载 · \(skin.rating_average, specifier: "%.1f") 分 · \(skin.rating_count) 人评分")
          .font(.subheadline).foregroundStyle(.secondary)
        Button { run {
          let design = try await SkinCommunityAPI.shared.download(skin.id)
          var library = CustomSkinLibrary.designs
          let id = UUID(uuidString: skin.id) ?? UUID()
          if let index = library.firstIndex(where: { $0.id == id }) { library[index].design = design }
          else {
            guard library.count < 12 else { throw CommunityFailure(message: "本地皮肤已满，请在「我的设计」删除一款后重试。") }
            library.append(SavedKeyboardSkin(id: id, name: skin.name, design: design))
          }
          guard CustomSkinLibrary.save(library) else { throw CommunityFailure(message: "无法保存皮肤，请检查设备存储。") }
          trial = try KeyboardSkinTrialStore().begin(name: skin.name, design: design)
          updated = try? await SkinCommunityAPI.shared.detail(skin.id)
        } } label: { Label("下载并试用", systemImage: "arrow.down.circle.fill").frame(maxWidth: .infinity) }
          .buttonStyle(.borderedProminent).disabled(busy).accessibilityIdentifier("downloadCommunitySkin")
        if !skin.owned {
          Text("我的评分（下载后可评，可重新选择）").font(.subheadline)
          HStack {
            ForEach(1...5, id: \.self) { stars in
              Button { run {
                try await SkinCommunityAPI.shared.rate(skin.id, stars: stars)
                updated = try await SkinCommunityAPI.shared.detail(skin.id)
              } } label: { Image(systemName: stars <= skin.my_rating ? "star.fill" : "star").frame(width: 44, height: 44) }
                .accessibilityLabel("评 \(stars) 星").disabled(busy)
            }
          }
        } else {
          Button("下架这款皮肤", role: .destructive) { confirmsRemoval = true }.disabled(busy)
        }
        if busy { ProgressView() }
      }.padding()
    }.navigationTitle("皮肤详情").navigationBarTitleDisplayMode(.inline)
      .task { run { updated = try await SkinCommunityAPI.shared.detail(initial.id) } }
      .sheet(item: $trial, onDismiss: {
        do { try KeyboardSkinTrialStore().restorePending() } catch { message = error.localizedDescription }
      }) { CommunitySkinTrialView(trial: $0) }
      .confirmationDialog("下架后其他用户无法再下载，已下载的本地皮肤会保留。", isPresented: $confirmsRemoval, titleVisibility: .visible) {
        Button("下架", role: .destructive) { run { try await SkinCommunityAPI.shared.unpublish(skin.id); dismiss() } }
      }
      .alert("皮肤社区", isPresented: Binding(get: { message != nil }, set: { if !$0 { message = nil } })) { Button("好", role: .cancel) {} } message: { Text(message ?? "") }
  }
  private func run(_ action: @escaping @MainActor () async throws -> Void) {
    guard !busy else { return }; busy = true
    Task { defer { busy = false }; do { try await action() } catch { message = error.localizedDescription } }
  }
}

struct CommunityPublishView: View {
  var onPublished: () -> Void
  var selectedSkinID: UUID? = nil
  @Environment(\.dismiss) private var dismiss
  @State private var library = CustomSkinLibrary.designs
  @State private var selected = UUID()
  @State private var publicationID = UUID().uuidString.lowercased()
  @State private var name = ""
  @State private var description = ""
  @State private var agrees = false
  @State private var busy = false
  @State private var message: String?
  private var design: CustomKeyboardSkin? { library.first { $0.id == selected }?.design }

  /// The library is stored in an App Group JSON file, so returning from the editor needs an explicit refresh.
  private func refreshLibrary(preferring preferred: UUID? = nil) {
    library = CustomSkinLibrary.designs
    guard library.first(where: { $0.id == selected }) == nil else { return }
    if let first = library.first(where: { $0.id == preferred }) ?? library.first {
      selected = first.id
      name = first.name
      publicationID = UUID().uuidString.lowercased()
    }
  }
  var body: some View {
    NavigationView {
      Form {
        Section {
          if library.isEmpty {
            Text("还没有保存的设计。先设计一款并命名保存，回到这里就能发布它。")
              .foregroundStyle(.secondary)
          } else {
            Picker("我的皮肤", selection: $selected) { ForEach(library) { Text($0.name).tag($0.id) } }
              .onChange(of: selected) { id in name = library.first { $0.id == id }?.name ?? ""; publicationID = UUID().uuidString.lowercased() }
            if let design { CommunityDesignPreview(design: design) }
          }
          if selectedSkinID == nil {
            NavigationLink {
              CustomSkinEditorView(publishable: false).onDisappear { refreshLibrary() }
            } label: {
              SettingsRowLabel(title: library.isEmpty ? "去设计一款" : "继续编辑我的皮肤",
                               detail: "在编辑器里调好，到「我的」命名保存",
                               symbol: "paintbrush.pointed.fill")
            }
            .accessibilityIdentifier("designSkinFromPublish")
          }
        } header: {
          Text("选择已保存的设计")
        }
        Section("发布信息") {
          TextField("皮肤名称（最多 32 字）", text: $name).onChange(of: name) { name = String($0.prefix(32)); publicationID = UUID().uuidString.lowercased() }
          TextField("设计说明（最多 280 字）", text: $description).onChange(of: description) { description = String($0.prefix(280)); publicationID = UUID().uuidString.lowercased() }
          Toggle("我拥有发布所用素材的权利，并同意其他用户免费下载使用", isOn: $agrees)
          Text("发布后，设计及照片壁纸将上传并公开。请勿包含私人照片或敏感信息。作者可随时下架。").font(.caption).foregroundStyle(.secondary)
        }
        Button("发布到社区") {
          guard let design else { return }; busy = true
          Task {
            defer { busy = false }
            do {
              try await SkinCommunityAPI.shared.publish(id: publicationID, name: name, description: description, design: design)
              onPublished(); dismiss()
            } catch { message = error.localizedDescription }
          }
        }.disabled(busy || !agrees || design == nil || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
          .accessibilityIdentifier("confirmCommunitySkinPublication")
        if busy { ProgressView() }
      }.disabled(busy)
        .navigationTitle("发布皮肤")
        .toolbar { ToolbarItem(placement: .cancellationAction) { Button("取消") { dismiss() }.disabled(busy) } }
        .onAppear { refreshLibrary(preferring: selectedSkinID) }
        .alert("发布失败", isPresented: Binding(get: { message != nil }, set: { if !$0 { message = nil } })) { Button("好", role: .cancel) {} } message: { Text(message ?? "") }
    }.interactiveDismissDisabled(busy)
  }
}

// A value-based preview must not change the user's active custom skin while browsing.
struct CommunityDesignPreview: View {
  let design: CustomKeyboardSkin
  var nineKey = false
  private func color(_ rgb: UInt32) -> Color { Color(uiColor: CustomKeyboardSkin.color(rgb)) }
  var body: some View { KeyboardPreviewCanvas { keyboard }.accessibilityHidden(true) }
  private var keyboard: some View {
    VStack(spacing: 6) {
      HStack { Text("你好"); Text("你号"); Spacer(); Text(nineKey ? "九键" : "全拼") }.font(.caption).foregroundStyle(color(design.accent)).frame(height: 32)
      if nineKey {
        HStack(spacing: 4) {
          VStack(spacing: 4) { key("，"); key("。"); key("？"); key("！") }.frame(width: 32)
          VStack(spacing: 4) {
            ForEach([["分词", "ABC", "DEF"], ["GHI", "JKL", "MNO"], ["PQRS", "TUV", "WXYZ"]], id: \.self) { row in
              HStack(spacing: 4) { ForEach(row, id: \.self) { key($0) } }
            }
          }
          VStack(spacing: 4) { key("⌫"); key("."); key("0") }.frame(width: 36)
        }.frame(maxHeight: .infinity)
      } else {
        ForEach(["QWERTYUIOP", "ASDFGHJKL", "⇧ZXCVBNM⌫"], id: \.self) { row in
          HStack(spacing: 3) { ForEach(Array(row).map(String.init), id: \.self) { key($0) } }
        }
      }
      HStack(spacing: 3) { key("123"); key("，"); key("空格").frame(minWidth: 100); key("↵") }.frame(height: 44)
    }.padding(10).background {
      KeyboardSkinBackdrop(skin: .custom, design: design)
    }.clipShape(RoundedRectangle(cornerRadius: 12)).accessibilityHidden(true)
  }
  private func key(_ text: String) -> some View {
    Text(text).font(.system(size: 14, weight: .medium, design: design.monospaced ? .monospaced : .default))
      .foregroundStyle(color(text == "↵" ? CustomKeyboardSkin.readableText(on: design.actionBackground) : design.keyForeground)).frame(maxWidth: .infinity, maxHeight: .infinity)
      .background { SkinKeySurface(design: design, action: text == "↵") }
  }
}
