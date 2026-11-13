import SwiftUI
import AppKit
import FileIDShared

private let reviewSpring = Animation.spring(response: 0.38, dampingFraction: 0.8)

struct FaceReviewList: View {
    let model: FaceReviewModel
    let personByID: [Int64: ReadStore.PersonRow]
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let message = model.message { FaceReviewMessage(text: message) }
            if model.undoOperationID != nil { FaceReviewUndoBar(model: model) }
            if model.personID != nil, model.suggestedCount > 0 { acceptAll }
            ForEach(model.items) { item in
                FaceReviewRow(item: item, candidate: personByID[item.personID], model: model)
                    .transition(.opacity.combined(with: .move(edge: .leading)))
            }
            if model.hasMore { showMore }
            if model.loaded && model.items.isEmpty && model.undoOperationID == nil {
                MetaText(FaceReviewText.empty, color: Theme.text3)
            }
        }
        .animation(reduceMotion ? nil : reviewSpring, value: model.items.map(\.faceID))
        .animation(reduceMotion ? nil : reviewSpring, value: model.undoOperationID)
    }

    private var acceptAll: some View {
        let count = model.suggestedCount
        let person = model.items.first { $0.reason == "suggested" }?.personName
        return Button { Task { await model.acceptAll() } } label: {
            Label(FaceReviewText.acceptAllTitle(count), systemImage: "checkmark.circle")
        }
        .buttonStyle(.dsAccentOutline)
        .disabled(model.busy)
        .accessibilityLabel(FaceReviewText.acceptAllLabel(count, person))
        .help("Accept every strong suggestion for this person at once. Faces in photos that already have this person stay for review.")
    }

    private var showMore: some View {
        Button("Show more") { Task { await model.loadMore() } }
            .buttonStyle(.dsSecondary)
    }
}

private struct FaceReviewMessage: View {
    let text: String
    var body: some View {
        Label(text, systemImage: "exclamationmark.circle")
            .font(Theme.Font.body(12.5))
            .foregroundStyle(Theme.warning)
    }
}

private struct FaceReviewUndoBar: View {
    let model: FaceReviewModel
    var body: some View {
        HStack(spacing: 10) {
            MetaText(model.notice ?? FaceReviewText.saved, color: Theme.text2)
            Spacer()
            Button(FaceReviewText.undo) { Task { await model.undo() } }
                .buttonStyle(.dsSecondary)
                .disabled(model.busy)
                .accessibilityLabel(FaceReviewText.undoLabel)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(Theme.goldWash, in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        .transition(.opacity)
    }
}

private struct FaceReviewRow: View {
    let item: FaceReviewItem
    let candidate: ReadStore.PersonRow?
    let model: FaceReviewModel

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            FaceReviewCrop(path: item.path, bbox: item.bbox, revision: nil, label: FaceReviewText.faceLabel(item))
            Image(systemName: "arrow.right").foregroundStyle(Theme.text3).accessibilityHidden(true)
            FaceReviewCrop(path: candidate?.representativePath, bbox: candidate?.representativeBBox,
                           revision: candidate?.representativeCacheRevision, label: FaceReviewText.name(item.personName))
            details
            Spacer(minLength: 8)
            actions
        }
        .padding(10)
        .background(Theme.pane, in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.hairline, lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(FaceReviewText.rowLabel(item))
    }

    private var details: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(FaceReviewText.name(item.personName).capitalizedFirst)
                .font(Theme.Font.body(14, .semibold))
                .foregroundStyle(item.personName == nil ? Theme.text3 : Theme.text1)
            Text(FaceReviewText.reason(item))
                .font(Theme.Font.body(12.5))
                .foregroundStyle(Theme.text2)
                .fixedSize(horizontal: false, vertical: true)
                .help(FaceReviewText.scoreHelp(item.score))
            MetaText(item.lowQuality == true ? "\(FaceReviewText.fileName(item)) · \(FaceReviewText.lowQuality)" : FaceReviewText.fileName(item), size: 11)
        }
    }

    private var actions: some View {
        HStack(spacing: 8) {
            button("Ignore", .ignore, style: .ghost, label: FaceReviewText.ignoreLabel)
                .help(FaceReviewText.ignoreHelp)
            button("Not this person", .reject, style: .secondary, label: FaceReviewText.rejectLabel(item))
                .help(FaceReviewText.rejectHelp(item))
            button("Accept", .accept, style: .primary, label: FaceReviewText.acceptLabel(item))
                .help(FaceReviewText.acceptHelp)
        }
        .disabled(model.busy)
    }

    private enum Kind { case primary, secondary, ghost }

    @ViewBuilder
    private func button(_ title: String, _ resolution: FaceReviewResolution, style: Kind, label: String) -> some View {
        let action = { Task { await model.resolve(item, resolution) } }
        switch style {
        case .primary:
            Button(title) { _ = action() }.buttonStyle(.dsPrimary).tint(Theme.gold).accessibilityLabel(label)
        case .secondary:
            Button(title) { _ = action() }.buttonStyle(.dsSecondary).accessibilityLabel(label)
        case .ghost:
            Button(title) { _ = action() }.buttonStyle(.dsGhost).accessibilityLabel(label)
        }
    }
}

private extension String {
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}

struct FaceReviewCrop: View {
    let path: String?
    let bbox: String?
    let revision: String?
    let label: String
    var side: CGFloat = 56
    @State private var image: NSImage?

    var body: some View {
        Color.white.opacity(0.04)
            .frame(width: side, height: side)
            .overlay(face)
            .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairline, lineWidth: 1))
            .task(id: key) { image = await FaceReviewCropLoader.crop(path: path, bbox: bbox, revision: revision, key: key) }
            .accessibilityElement()
            .accessibilityLabel(label)
            .accessibilityAddTraits(.isImage)
    }

    private var key: String { [path ?? "", bbox ?? "", revision ?? ""].joined(separator: "|") }

    @ViewBuilder
    private var face: some View {
        if let image {
            Image(nsImage: image).resizable().scaledToFill()
        } else {
            Image(systemName: "person.crop.square").foregroundStyle(Theme.text3)
        }
    }
}

@MainActor
enum FaceReviewCropLoader {
    static func crop(path: String?, bbox: String?, revision: String?, key: String) async -> NSImage? {
        let cacheKey = "review|" + key
        if let cached = FaceCropCache.image(for: cacheKey) { return cached }
        guard let path else { return nil }
        let url = URL(fileURLWithPath: path)
        guard let full = await ThumbnailService.shared.thumbnail(for: url, size: 360, revision: revision),
              !Task.isCancelled else { return nil }
        guard let bbox else { return full }
        var orientation = 1
        if bbox.trimmingCharacters(in: .whitespacesAndNewlines).hasPrefix("{") {
            guard let value = await ThumbnailService.sourceOrientation(for: url) else { return full }
            orientation = value
        }
        let box = UncheckedImage(full)
        let cropped = await Task.detached(priority: .userInitiated) {
            ThumbnailService.cropFace(in: box.image, bbox: bbox, sourceOrientation: orientation).map(UncheckedImage.init)
        }.value
        let face = cropped?.image ?? full
        FaceCropCache.store(face, for: cacheKey)
        return face
    }
}

struct FaceReviewSheet: View {
    let model: FaceReviewModel
    let merges: FaceQueueModel
    let splits: FaceQueueModel
    let personByID: [Int64: ReadStore.PersonRow]
    let store: ReadStore
    let onDone: () -> Void
    @State private var queue: FaceQueueKind = .faces

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .firstTextBaseline) {
                Text(FaceReviewText.title).font(Theme.Font.body(20, .semibold))
                    .accessibilityLabel(FaceReviewText.header(waiting))
                MetaText("\(waiting) waiting")
                Spacer()
                Button("Done", action: onDone).buttonStyle(.dsPrimary).keyboardShortcut(.defaultAction)
            }
            .padding(16)
            picker.padding(.horizontal, 16).padding(.bottom, 12)
            Divider()
            ScrollView { content.padding(16) }
        }
        .frame(minWidth: 760, minHeight: 520)
        .task { await loadAll() }
    }

    private var waiting: Int { model.total + merges.total + splits.total }

    private var picker: some View {
        Picker("Queue", selection: $queue) {
            ForEach(FaceQueueKind.allCases) { kind in Text("\(kind.title) (\(count(kind)))").tag(kind) }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .accessibilityLabel("Which suggestions to review")
    }

    private func count(_ kind: FaceQueueKind) -> Int {
        switch kind {
        case .faces: return model.total
        case .merges: return merges.total
        case .splits: return splits.total
        }
    }

    @ViewBuilder
    private var content: some View {
        switch queue {
        case .faces: FaceReviewList(model: model, personByID: personByID)
        case .merges: FaceQueueList(model: merges, personByID: personByID, store: store)
        case .splits: FaceQueueList(model: splits, personByID: personByID, store: store)
        }
    }

    private func loadAll() async {
        await model.reload()
        await merges.reload()
        await splits.reload()
        if model.total == 0 { queue = merges.total > 0 ? .merges : (splits.total > 0 ? .splits : .faces) }
    }
}

struct FaceReviewSection: View {
    let person: ReadStore.PersonRow
    let onChange: () -> Void
    @State private var model: FaceReviewModel

    init(engine: EngineClient, person: ReadStore.PersonRow, onChange: @escaping () -> Void) {
        self.person = person
        self.onChange = onChange
        _model = State(initialValue: FaceReviewModel(engine: engine, personID: person.id))
    }

    var body: some View {
        Group {
            if model.isVisible {
                VStack(alignment: .leading, spacing: 8) {
                    SectionLabel(FaceReviewText.title)
                        .accessibilityLabel(FaceReviewText.header(model.total))
                    FaceReviewList(model: model, personByID: [person.id: person])
                }
                .padding(.horizontal, 16).padding(.bottom, 12)
            }
        }
        .task { await model.reload() }
        .onChange(of: model.changes) { _, _ in onChange() }
    }
}
