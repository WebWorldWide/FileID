import SwiftUI
import FileIDShared

private let queueSpring = Animation.spring(response: 0.38, dampingFraction: 0.8)

struct FaceQueueList: View {
    let model: FaceQueueModel
    let personByID: [Int64: ReadStore.PersonRow]
    let store: ReadStore
    @State private var crops: [Int64: ReadStore.FaceCrop] = [:]
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let message = model.message {
                Label(message, systemImage: "exclamationmark.circle")
                    .font(Theme.Font.body(12.5)).foregroundStyle(Theme.warning)
            }
            if model.undoOperationID != nil { FaceQueueUndoBar(model: model) }
            rows
            if model.loaded && model.isEmpty && model.undoOperationID == nil {
                MetaText(model.kind == .merges ? FaceQueueText.mergesEmpty : FaceQueueText.splitsEmpty, color: Theme.text3)
            }
        }
        .animation(reduceMotion ? nil : queueSpring, value: model.changes)
        .task(id: previewIDs) { crops = store.faceCrops(previewIDs) }
    }

    @ViewBuilder
    private var rows: some View {
        ForEach(model.merges) { item in
            FaceMergeRow(item: item, keep: personByID[item.personID], other: personByID[item.otherPersonID], model: model)
                .transition(.opacity.combined(with: .move(edge: .leading)))
        }
        ForEach(model.splits) { item in
            FaceSplitRow(item: item, crops: crops, model: model)
                .transition(.opacity.combined(with: .move(edge: .leading)))
        }
    }

    private var previewIDs: [Int64] { model.splits.flatMap { $0.keepFaceIDs + $0.splitFaceIDs } }
}

private struct FaceQueueUndoBar: View {
    let model: FaceQueueModel
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

private struct FaceQueueCard<Content: View>: View {
    let label: String
    @ViewBuilder let content: Content
    var body: some View {
        content
            .padding(10)
            .background(Theme.pane, in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.hairline, lineWidth: 1))
            .accessibilityElement(children: .contain)
            .accessibilityLabel(label)
    }
}

private struct FaceMergeRow: View {
    let item: FaceMergeSuggestion
    let keep: ReadStore.PersonRow?
    let other: ReadStore.PersonRow?
    let model: FaceQueueModel

    var body: some View {
        FaceQueueCard(label: FaceQueueText.mergeRow(item)) {
            HStack(alignment: .center, spacing: 12) {
                crop(other, item.otherName)
                Image(systemName: "arrow.right").foregroundStyle(Theme.text3).accessibilityHidden(true)
                crop(keep, item.personName)
                details
                Spacer(minLength: 8)
                actions
            }
        }
    }

    private func crop(_ person: ReadStore.PersonRow?, _ name: String?) -> some View {
        FaceReviewCrop(path: person?.representativePath, bbox: person?.representativeBBox,
                       revision: person?.representativeCacheRevision, label: FaceQueueText.name(name))
    }

    private var details: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("\(FaceQueueText.name(item.otherName)) → \(FaceQueueText.name(item.personName))")
                .font(Theme.Font.body(14, .semibold)).foregroundStyle(Theme.text1)
            Text(FaceQueueText.mergeReason(item))
                .font(Theme.Font.body(12.5)).foregroundStyle(Theme.text2)
                .fixedSize(horizontal: false, vertical: true)
            MetaText("\(FaceQueueText.faces(item.otherFaceCount)) · \(FaceQueueText.faces(item.faceCount))", size: 11)
        }
    }

    private var actions: some View {
        HStack(spacing: 8) {
            Button("Dismiss") { Task { await model.resolve(item, "dismiss") } }
                .buttonStyle(.dsGhost).accessibilityLabel(FaceQueueText.dismissMergeLabel(item))
            Button("Not the same person") { Task { await model.resolve(item, "notSamePerson") } }
                .buttonStyle(.dsSecondary).accessibilityLabel(FaceQueueText.notSameLabel(item))
            Button("Merge") { Task { await model.resolve(item, "merge") } }
                .buttonStyle(.dsPrimary).tint(Theme.gold).accessibilityLabel(FaceQueueText.mergeLabel(item))
        }
        .disabled(model.busy)
    }
}

private struct FaceSplitRow: View {
    let item: FaceSplitSuggestion
    let crops: [Int64: ReadStore.FaceCrop]
    let model: FaceQueueModel

    var body: some View {
        FaceQueueCard(label: FaceQueueText.splitRow(item)) {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .firstTextBaseline) {
                    Text(FaceQueueText.name(item.personName)).font(Theme.Font.body(14, .semibold)).foregroundStyle(Theme.text1)
                    MetaText(FaceQueueText.splitReason(item), size: 12)
                    Spacer(minLength: 8)
                    actions
                }
                strip(FaceQueueText.stays(item), item.keepFaceIDs)
                strip(FaceQueueText.moves(item), item.splitFaceIDs)
            }
        }
    }

    private func strip(_ title: String, _ faces: [Int64]) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            MetaText(title, size: 11)
            HStack(spacing: 6) {
                ForEach(faces, id: \.self) { face in
                    FaceReviewCrop(path: crops[face]?.path, bbox: crops[face]?.bbox, revision: nil, label: title, side: 44)
                }
            }
        }
    }

    private var actions: some View {
        HStack(spacing: 8) {
            Button("Dismiss") { Task { await model.resolve(item, "dismiss") } }
                .buttonStyle(.dsGhost).accessibilityLabel(FaceQueueText.dismissSplitLabel(item))
            Button("Split into two") { Task { await model.resolve(item, "split") } }
                .buttonStyle(.dsPrimary).tint(Theme.gold).accessibilityLabel(FaceQueueText.splitLabel(item))
        }
        .disabled(model.busy)
    }
}

struct FaceRefreshRow: View {
    let model: FaceRefreshModel

    var body: some View {
        if let line = model.line {
            HStack(spacing: 10) {
                Image(systemName: "arrow.triangle.2.circlepath").foregroundStyle(Theme.gold).accessibilityHidden(true)
                Text(line).font(Theme.Font.body(13)).foregroundStyle(Theme.text2)
                    .accessibilityLabel(line)
                Spacer(minLength: 8)
                controls
            }
            .disabled(model.busy)
            .transition(.opacity)
        }
    }

    @ViewBuilder
    private var controls: some View {
        if model.isRunning {
            Button("Pause") { Task { await model.pause() } }.buttonStyle(.dsSecondary).accessibilityLabel("Pause updating faces")
            Button("Cancel") { Task { await model.cancel() } }.buttonStyle(.dsGhost).accessibilityLabel("Cancel updating faces")
        } else if model.isPaused {
            Button("Resume") { Task { await model.resume() } }.buttonStyle(.dsSecondary).accessibilityLabel("Resume updating faces")
            Button("Cancel") { Task { await model.cancel() } }.buttonStyle(.dsGhost).accessibilityLabel("Cancel updating faces")
        } else if model.canStart {
            Button(FaceRefreshText.update) { Task { await model.start() } }
                .buttonStyle(.dsAccentOutline).accessibilityLabel(FaceRefreshText.updateLabel)
        }
    }
}
