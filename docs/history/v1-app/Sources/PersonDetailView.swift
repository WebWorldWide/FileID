import SwiftUI
import SwiftData
import AppKit

// MARK: - PersonDetailView

// Full-screen overlay. Shows every file clustered under a given PersonRecord
// with a multi-select grid. The "Not this person" action removes selected
// files from the cluster and re-clusters those face prints against every
// OTHER identity (fallback: leave unclustered).

struct PersonDetailView: View {
    let person: PersonRecord
    @ObservedObject var viewModel: AppViewModel
    @Environment(\.modelContext) private var context

    @State private var files: [FileRecord] = []
    @State private var selection: Set<UUID> = []
    @State private var lastTappedID: UUID?  // anchor for shift-click range select
    @State private var isLoading = true
    @State private var isReassigning = false
    @State private var namingText: String = ""
    @State private var isEditingName = false
    @State private var confirmDelete = false

    private let columns = [GridItem(.adaptive(minimum: 140), spacing: 10)]

    var body: some View {
        ZStack {
            Color.black.opacity(0.85).ignoresSafeArea()
                .onTapGesture { if selection.isEmpty { viewModel.closePersonDetail() } }

            VStack(spacing: 0) {
                header
                Divider().background(Color.white.opacity(0.08))
                content
                footer
            }
            .frame(maxWidth: 1280, maxHeight: .infinity)
            .background(Color.black.opacity(0.6))
        }
        .task(id: person.id) { await loadFiles() }
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: 14) {
            Button { viewModel.closePersonDetail() } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.title2)
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .contentShape(Rectangle())
            .help("Close")

            ZStack {
                if let data = person.representativeFaceCropData, let img = NSImage(data: data) {
                    Image(nsImage: img)
                        .resizable()
                        .scaledToFill()
                        .frame(width: 54, height: 54)
                        .clipShape(Circle())
                        .overlay(Circle().stroke(Theme.gold, lineWidth: 2))
                } else {
                    Circle()
                        .fill(Color.gray.opacity(0.3))
                        .frame(width: 54, height: 54)
                        .overlay(Image(systemName: "person.fill").font(.title3))
                }
            }

            VStack(alignment: .leading, spacing: 2) {
                if isEditingName {
                    TextField("Name", text: $namingText, onCommit: commitName)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 240)
                } else {
                    HStack(spacing: 8) {
                        Text(person.name ?? "Unknown Person")
                            .font(.title2.bold())
                        Button {
                            namingText = person.name ?? ""
                            isEditingName = true
                        } label: {
                            Image(systemName: "pencil")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                        .contentShape(Rectangle())
                        .help("Rename")
                    }
                }
                Text("\(files.count) photo\(files.count == 1 ? "" : "s")")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Spacer()

            if !selection.isEmpty {
                Text("\(selection.count) selected")
                    .font(.caption.bold())
                    .foregroundStyle(Theme.gold)
                Button("Clear") { selection.removeAll() }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .help("Clear selection")
            } else {
                Button("Select All") { selection = Set(files.map { $0.id }) }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(files.isEmpty)
                    .help("Select every photo in this cluster")
            }
        }
        .padding(16)
    }

    // MARK: - Grid

    @ViewBuilder
    private var content: some View {
        if isLoading {
            VStack {
                Spacer()
                ProgressView().progressViewStyle(.circular)
                Text("Loading photos…")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.top, 8)
                Spacer()
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if files.isEmpty {
            VStack(spacing: 12) {
                Spacer()
                Image(systemName: "photo.stack")
                    .font(.system(size: 52))
                    .foregroundStyle(.tertiary)
                Text("No photos in this cluster yet.")
                    .font(.headline)
                    .foregroundStyle(.secondary)
                Text("New faces will appear here as scans complete.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                Spacer()
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView {
                LazyVGrid(columns: columns, spacing: 10) {
                    ForEach(files, id: \.id) { file in
                        PersonPhotoCell(
                            file: file,
                            selected: selection.contains(file.id),
                            onToggle: { handleTap(file.id) },
                            onOpen: { openInPreview(file) }
                        )
                    }
                }
                .padding(16)
            }
        }
    }

    // Shift-click handler: select every file between the last-tapped anchor
    // and the one the user just clicked. Works in both directions.
    private func extendSelection(to id: UUID) {
        guard let anchor = lastTappedID,
              let anchorIdx = files.firstIndex(where: { $0.id == anchor }),
              let endIdx    = files.firstIndex(where: { $0.id == id }) else {
            toggle(id)
            return
        }
        let range = anchorIdx <= endIdx ? anchorIdx...endIdx : endIdx...anchorIdx
        for i in range { selection.insert(files[i].id) }
    }

    // MARK: - Footer

    private var footer: some View {
        HStack(spacing: 12) {
            Button(role: .destructive) {
                confirmDelete = true
            } label: {
                Label("Delete Person", systemImage: "person.fill.xmark")
            }
            .buttonStyle(.bordered)
            .help("Remove this person and all clustering. Files stay intact.")

            Spacer()

            if !selection.isEmpty {
                Button {
                    reassignSelected()
                } label: {
                    if isReassigning {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small)
                            Text("Reassigning…")
                        }
                    } else {
                        Label("Not this person (\(selection.count))", systemImage: "person.crop.circle.badge.minus")
                            .fontWeight(.semibold)
                    }
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.gold)
                .foregroundStyle(.black)
                .disabled(isReassigning)
                .help("Remove these photos from this person and re-cluster them")
            }
        }
        .padding(16)
        .background(.ultraThinMaterial)
        .alert("Delete this person?", isPresented: $confirmDelete) {
            Button("Cancel", role: .cancel) {}
            Button("Delete", role: .destructive) { deletePerson() }
        } message: {
            Text("Files stay on disk — only the clustering for \(person.name ?? "this person") is removed.")
        }
    }

    // MARK: - Actions

    private func toggle(_ id: UUID) {
        if selection.contains(id) {
            selection.remove(id)
        } else {
            selection.insert(id)
        }
        lastTappedID = id
    }

    // Called by PersonPhotoCell — detects modifier keys via NSEvent since
    // SwiftUI's tap gesture doesn't expose them. Shift-click range-selects
    // from the last-tapped anchor; plain click single-toggles.
    private func handleTap(_ id: UUID) {
        let flags = NSEvent.modifierFlags
        if flags.contains(.shift) {
            extendSelection(to: id)
        } else {
            toggle(id)
        }
    }

    private func openInPreview(_ file: FileRecord) {
        viewModel.openPreview(file, in: files)
    }

    private func commitName() {
        let trimmed = namingText.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else {
            isEditingName = false
            return
        }
        let personID = person.id
        // Route through FaceClusteringService so the name fans out as a
        // `person:<name>` tag on every clustered file. Without this the
        // name would be a label only — Library search wouldn't surface it.
        Task {
            _ = try? await FaceClusteringService.shared.renamePerson(
                id: personID, newName: trimmed
            )
        }
        isEditingName = false
    }

    private func reassignSelected() {
        let ids = Array(selection)
        let personID = person.id
        isReassigning = true
        Task {
            await FaceClusteringService.shared.reassignFiles(from: personID, fileIDs: ids)

            // Was: `return` inside a MainActor.run closure, which only exits
            // the closure — the Task kept going and called `loadFiles()` on
            // a PersonRecord that `reassignFiles` may have just
            // `modelContext.delete`'d. That's the "Not this person" crash.
            // Fix: check existence outside the closure and exit the Task
            // cleanly when the person no longer exists.
            let personStillExists: Bool = await MainActor.run {
                selection.removeAll()
                isReassigning = false
                let desc = FetchDescriptor<PersonRecord>(
                    predicate: #Predicate { $0.id == personID }
                )
                return (try? context.fetch(desc).first) != nil
            }
            guard personStillExists else {
                await MainActor.run { viewModel.closePersonDetail() }
                return
            }
            await loadFiles()
        }
    }

    private func deletePerson() {
        let personID = person.id
        Task {
            // Removing every file from the person triggers the empty-person
            // deletion path inside reassignFiles.
            await FaceClusteringService.shared.reassignFiles(
                from: personID, fileIDs: person.fileIDs)
            await MainActor.run { viewModel.closePersonDetail() }
        }
    }

    // MARK: - Fetch

    private func loadFiles() async {
        isLoading = true
        let ids = Set(person.fileIDs)
        if ids.isEmpty {
            // Fallback for pre-backfill libraries: use the sample URLs.
            let urls = person.sampleFileURLs
            let desc = FetchDescriptor<FileRecord>(
                predicate: #Predicate { urls.contains($0.url) },
                sortBy: [SortDescriptor(\.creationDate, order: .reverse)]
            )
            files = (try? context.fetch(desc)) ?? []
        } else {
            let desc = FetchDescriptor<FileRecord>(
                predicate: #Predicate { ids.contains($0.id) },
                sortBy: [SortDescriptor(\.creationDate, order: .reverse)]
            )
            files = (try? context.fetch(desc)) ?? []
        }
        isLoading = false
    }
}

// MARK: - PersonPhotoCell

private struct PersonPhotoCell: View {
    @Bindable var file: FileRecord
    let selected: Bool
    let onToggle: () -> Void
    let onOpen: () -> Void

    var body: some View {
        ZStack(alignment: .topLeading) {
            ThumbnailView(url: file.url)
                .aspectRatio(1, contentMode: .fill)
                .frame(minWidth: 0, maxWidth: .infinity, minHeight: 140, maxHeight: 140)
                .clipped()
                .clipShape(RoundedRectangle(cornerRadius: 10))
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(selected ? Theme.gold : Color.white.opacity(0.08),
                                lineWidth: selected ? 3 : 1)
                )
                .onTapGesture(count: 2) { onOpen() }
                .onTapGesture { onToggle() }

            Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 20))
                .foregroundStyle(selected ? Theme.gold : Color.white.opacity(0.7))
                .background(Circle().fill(Color.black.opacity(0.45)))
                .padding(6)
                .onTapGesture { onToggle() }

            VStack { Spacer()
                Text(file.filename)
                    .font(.system(size: 10))
                    .lineLimit(1)
                    .padding(4)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(
                        LinearGradient(
                            colors: [.black.opacity(0), .black.opacity(0.75)],
                            startPoint: .top, endPoint: .bottom
                        )
                    )
            }
            .allowsHitTesting(false)
            .clipShape(RoundedRectangle(cornerRadius: 10))
        }
        .contentShape(Rectangle())
        .help(file.filename)
    }
}
