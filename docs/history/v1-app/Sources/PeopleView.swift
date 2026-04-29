import SwiftUI
import SwiftData

struct PeopleView: View {
    @ObservedObject var viewModel: AppViewModel
    @Environment(\.modelContext) private var context
    // Bounded so an extreme library (low-quality face detection producing
    // 10K+ identities) doesn't try to materialize them all into the grid.
    // The hard maxIdentities cap in FaceClusteringService is 2 000, so this
    // is a defense-in-depth ceiling.
    @Query(PeopleView.identityDescriptor) private var identities: [PersonRecord]

    private static let identityDescriptor: FetchDescriptor<PersonRecord> = {
        var d = FetchDescriptor<PersonRecord>(
            sortBy: [SortDescriptor(\PersonRecord.faceCount, order: .reverse)]
        )
        d.fetchLimit = 5_000
        return d
    }()

    @State private var selectedIdentity: PersonRecord?
    @State private var namingText: String = ""
    @State private var isNaming = false
    @State private var mergeSource: PersonRecord?
    @State private var searchText: String = ""
    @State private var sortOption: SortOption = .byCount
    @State private var suggestedPairs: [(PersonRecord, PersonRecord)] = []
    // Cache invalidated via .onChange hooks; was a computed property which
    // re-ran filter+sort on every body eval (~5-10 ms hitch at 5K identities).
    @State private var cachedFiltered: [PersonRecord] = []
    // Debounces search-text changes so each keystroke doesn't refilter the
    // whole identity list while the user is still typing.
    @State private var searchDebounceTask: Task<Void, Never>?

    enum SortOption: String, CaseIterable {
        case byCount = "Most Photos"
        case byName = "Name"
    }

    private func recomputeFiltered() {
        var result = identities
        if !searchText.isEmpty {
            let needle = searchText.lowercased()
            result = result.filter {
                ($0.name ?? "Unknown").lowercased().contains(needle)
            }
        }
        switch sortOption {
        case .byCount: result.sort { $0.faceCount > $1.faceCount }
        case .byName:  result.sort { ($0.name ?? "zzz") < ($1.name ?? "zzz") }
        }
        cachedFiltered = result
    }

    var filteredIdentities: [PersonRecord] { cachedFiltered }

    // Loads suggested merge pairs by fetching UUIDs from FaceClusteringService
    // (Sendable) then resolving PersonRecords from the local context.
    private func loadSuggestedPairs() async {
        let uuidPairs = (try? await FaceClusteringService.shared.suggestedMerges()) ?? []
        guard !uuidPairs.isEmpty else { suggestedPairs = []; return }
        let all = (try? context.fetch(FetchDescriptor<PersonRecord>())) ?? []
        let map = Dictionary(uniqueKeysWithValues: all.map { ($0.id, $0) })
        suggestedPairs = uuidPairs.compactMap { (a, b) -> (PersonRecord, PersonRecord)? in
            guard let ra = map[a], let rb = map[b] else { return nil }
            return (ra, rb)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header
            HStack {
                Image(systemName: "person.2.fill")
                    .font(.largeTitle)
                    .foregroundStyle(Theme.gold)
                VStack(alignment: .leading, spacing: 2) {
                    Text("People & Identities")
                        .font(.title.bold())
                    Text("\(identities.count) people detected")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()

                // Sort picker
                Picker("Sort", selection: $sortOption) {
                    ForEach(SortOption.allCases, id: \.self) { option in
                        Text(option.rawValue).tag(option)
                    }
                }
                .pickerStyle(.segmented)
                .fixedSize(horizontal: true, vertical: false)
                .frame(maxWidth: 280)
                .help("Sort people by name, photo count, or most recent appearance.")
            }
            .padding(.horizontal)
            .padding(.top, 20)
            .padding(.bottom, 12)

            // Search bar
            HStack {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Search people by name...", text: $searchText)
                    .textFieldStyle(.plain)
                if !searchText.isEmpty {
                    Button { searchText = "" } label: {
                        Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .contentShape(Rectangle())
                    .help("Clear search")
                }
            }
            .padding(8)
            .background(RoundedRectangle(cornerRadius: 8).fill(Color.white.opacity(0.1)))
            .padding(.horizontal)
            .padding(.bottom, 12)

            if !suggestedPairs.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Image(systemName: "arrow.triangle.merge")
                            .foregroundStyle(Theme.gold)
                        Text("Suggested Merges")
                            .font(.headline)
                        Text("(\(suggestedPairs.count))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button("Dismiss") { suggestedPairs = [] }
                            .buttonStyle(.plain)
                            .contentShape(Rectangle())
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .help("Dismiss all suggested merges")
                    }
                    .padding(.horizontal)

                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 12) {
                            ForEach(Array(suggestedPairs.enumerated()), id: \.offset) { _, pair in
                                HStack(spacing: 8) {
                                    SuggestedMergeMini(identity: pair.0)
                                    Image(systemName: "arrow.right")
                                        .foregroundStyle(.secondary)
                                        .font(.caption)
                                    SuggestedMergeMini(identity: pair.1)
                                    Button("Merge") {
                                        Task {
                                            try? await FaceClusteringService.shared.merge(
                                                sourceID: pair.0.id, targetID: pair.1.id)
                                            await loadSuggestedPairs()
                                        }
                                    }
                                    .buttonStyle(.bordered)
                                    .controlSize(.small)
                                    .help("Merge these two people into one")
                                }
                                .padding(10)
                                .background(RoundedRectangle(cornerRadius: 10).fill(.ultraThinMaterial))
                            }
                        }
                        .padding(.horizontal)
                    }
                }
                .padding(.bottom, 8)
            }

            if filteredIdentities.isEmpty {
                if identities.isEmpty && viewModel.clusteringFacesTotal > 0 {
                    clusteringProgressCard
                } else if identities.isEmpty && viewModel.isProcessing {
                    liveScanClusteringCard
                } else {
                    VStack {
                        Spacer()
                        Image(systemName: "face.dashed")
                            .font(.system(size: 60))
                            .foregroundStyle(Theme.gold)
                        Text(identities.isEmpty ? "No People Detected Yet" : "No Matches")
                            .font(.headline)
                            .foregroundStyle(.secondary)
                        Text(identities.isEmpty ?
                             "The AI is scanning your files for familiar faces. They will appear here automatically." :
                             "Try a different search term.")
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                            .multilineTextAlignment(.center)
                            .padding()
                        Spacer()
                    }
                    .frame(maxWidth: .infinity)
                }
            } else {
                ScrollView {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 200), spacing: 20)], spacing: 24) {
                        ForEach(filteredIdentities) { identity in
                            PersonCard(
                                identity: identity,
                                mergeSource: $mergeSource,
                                onOpen: {
                                    viewModel.openPersonDetail(identity)
                                },
                                onName: {
                                    selectedIdentity = identity
                                    namingText = identity.name ?? ""
                                    isNaming = true
                                },
                                onMerge: { source in
                                    merge(source: source, into: identity)
                                }
                            )
                        }
                    }
                    .padding()
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.opacity(0.3))
        .task {
            recomputeFiltered()
            await loadSuggestedPairs()
        }
        // Cache invalidation hooks. Search text is debounced 200 ms so
        // every keystroke doesn't refilter; the other inputs apply immediately.
        .onChange(of: searchText) { _, _ in
            searchDebounceTask?.cancel()
            searchDebounceTask = Task { @MainActor in
                try? await Task.sleep(for: .milliseconds(200))
                guard !Task.isCancelled else { return }
                recomputeFiltered()
            }
        }
        .onChange(of: sortOption)       { _, _ in recomputeFiltered() }
        .onChange(of: identities.count) { _, _ in recomputeFiltered() }
        // Refresh merge suggestions whenever a clustering pass completes.
        .onChange(of: viewModel.clusteringCompletedAt) { _, _ in
            recomputeFiltered()
            Task { await loadSuggestedPairs() }
        }
        // Clear stale suggestions the moment a new scan starts so the user
        // isn't looking at prior-scan pairs while mid-new-scan.
        .onChange(of: viewModel.isProcessing) { _, processing in
            if processing { suggestedPairs = [] }
        }
        .sheet(isPresented: $isNaming) {
            VStack(spacing: 20) {
                Text("Identify Person")
                    .font(.headline)

                if let data = selectedIdentity?.representativeFaceCropData, let nsImage = NSImage(data: data) {
                    Image(nsImage: nsImage)
                        .resizable()
                        .scaledToFill()
                        .frame(width: 80, height: 80)
                        .clipShape(Circle())
                }

                TextField("Name", text: $namingText)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 250)

                HStack {
                    Button("Cancel") { isNaming = false }
                        .help("Discard changes")
                    Button("Save") {
                        if let identity = selectedIdentity {
                            let id = identity.id
                            let name = namingText
                            // Propagate via FaceClusteringService so the name
                            // fans out as a `person:<name>` tag on every
                            // clustered file. Without this the name shows up
                            // on the card but search/filtering ignores it.
                            Task {
                                _ = try? await FaceClusteringService.shared
                                    .renamePerson(id: id, newName: name)
                            }
                            isNaming = false
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.gold)
                    .disabled(namingText.trimmingCharacters(in: .whitespaces).isEmpty)
                    .help("Save name for this person")
                }
            }
            .padding()
            .frame(width: 320, height: 260)
        }
    }

    // Live progress card shown while the post-scan clustering phase runs and
    // no PersonRecords have landed yet. Fades away the moment the `@Query`
    // starts returning identities.
    private var clusteringProgressCard: some View {
        let done  = viewModel.clusteringFacesDone
        let total = max(1, viewModel.clusteringFacesTotal)
        let pct   = min(1.0, Double(done) / Double(total))
        return VStack(spacing: 14) {
            Spacer()
            Image(systemName: "person.crop.circle.badge.clock")
                .font(.system(size: 56))
                .foregroundStyle(Theme.gold)
            Text("Clustering faces…")
                .font(.headline)
            Text("\(done) of \(viewModel.clusteringFacesTotal) face prints")
                .font(.caption)
                .foregroundStyle(.secondary)
                .monospacedDigit()
            ProgressView(value: pct)
                .progressViewStyle(.linear)
                .tint(Theme.gold)
                .frame(maxWidth: 320)
            Text("Identities appear as each batch completes.")
                .font(.caption2)
                .foregroundStyle(.tertiary)
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }

    // Shown while the main scan is still running but no PersonRecords have
    // landed yet — reassures the user that clustering is live, not waiting
    // for scan to finish.
    private var liveScanClusteringCard: some View {
        VStack(spacing: 14) {
            Spacer()
            Image(systemName: "sparkles.rectangle.stack")
                .font(.system(size: 56))
                .foregroundStyle(Theme.gold)
            Text("Clustering faces as scan runs…")
                .font(.headline)
            Text("People appear here live — first faces within ~1 minute.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal)
            ProgressView()
                .progressViewStyle(.circular)
                .controlSize(.small)
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }

    private func merge(source: PersonRecord, into target: PersonRecord) {
        Task {
            try? await FaceClusteringService.shared.merge(sourceID: source.id, targetID: target.id)
            mergeSource = nil
            await loadSuggestedPairs()
        }
    }
}

// MARK: - Person Card

struct PersonCard: View {
    let identity: PersonRecord
    @Binding var mergeSource: PersonRecord?
    let onOpen: () -> Void
    let onName: () -> Void
    let onMerge: (PersonRecord) -> Void

    var body: some View {
        VStack(spacing: 12) {
            // Face circle with count badge
            ZStack(alignment: .bottomTrailing) {
                if let data = identity.representativeFaceCropData, let nsImage = NSImage(data: data) {
                    Image(nsImage: nsImage)
                        .resizable()
                        .scaledToFill()
                        .frame(width: 100, height: 100)
                        .clipShape(Circle())
                        .overlay(Circle().stroke(
                            mergeSource?.id == identity.id ? Color.blue : Theme.gold,
                            lineWidth: 3
                        ))
                        .shadow(color: .black.opacity(0.3), radius: 8)
                } else if let firstURL = identity.sampleFileURLs.first {
                    // L3: use first sample thumbnail when face crop is unavailable
                    ThumbnailView(url: firstURL)
                        .frame(width: 100, height: 100)
                        .clipShape(Circle())
                        .overlay(Circle().stroke(
                            mergeSource?.id == identity.id ? Color.blue : Theme.gold,
                            lineWidth: 3
                        ))
                        .shadow(color: .black.opacity(0.3), radius: 8)
                } else {
                    Circle()
                        .fill(.gray.opacity(0.3))
                        .frame(width: 100, height: 100)
                        .overlay(Image(systemName: "person.fill").font(.title).foregroundStyle(.gray))
                }

                // Face count badge
                Text("\(identity.faceCount)")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundStyle(.black)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Capsule().fill(Theme.gold))
                    .offset(x: 4, y: 4)

                // Merge target button
                if mergeSource != nil && mergeSource?.id != identity.id {
                    Button {
                        onMerge(mergeSource!)
                    } label: {
                        Image(systemName: "arrow.triangle.merge")
                            .font(.system(size: 18, weight: .bold))
                            .foregroundStyle(.white)
                            .padding(6)
                            .background(Circle().fill(.blue))
                    }
                    .buttonStyle(.plain)
                    .contentShape(Rectangle())
                    .offset(x: 6, y: -40)
                    .help("Merge selected person into this one")
                }
            }

            Text(identity.name ?? "Unknown Person")
                .font(.system(size: 14, weight: .semibold))
                .lineLimit(1)

            Text("\(identity.faceCount) photo\(identity.faceCount == 1 ? "" : "s")")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)

            // Sample thumbnails
            if !identity.sampleFileURLs.isEmpty {
                HStack(spacing: 4) {
                    ForEach(identity.sampleFileURLs.prefix(4), id: \.self) { url in
                        ThumbnailView(url: url)
                            .frame(width: 36, height: 36)
                            .clipShape(RoundedRectangle(cornerRadius: 4))
                    }
                    if identity.sampleFileURLs.count > 4 {
                        Text("+\(identity.sampleFileURLs.count - 4)")
                            .font(.system(size: 9, weight: .bold))
                            .foregroundStyle(.secondary)
                    }
                }
            }

            // Action buttons
            HStack(spacing: 8) {
                Button("Name") { onName() }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .help("Rename this person")

                Button(mergeSource?.id == identity.id ? "Cancel" : "Merge") {
                    if mergeSource?.id == identity.id {
                        mergeSource = nil
                    } else {
                        mergeSource = identity
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .tint(mergeSource?.id == identity.id ? .red : .blue)
                .help(mergeSource?.id == identity.id
                      ? "Cancel merge"
                      : "Select this person as merge source, then click another to merge into")
            }
        }
        .padding(16)
        .background(
            RoundedRectangle(cornerRadius: 20)
                .fill(.ultraThinMaterial)
                .shadow(color: .black.opacity(0.15), radius: 8, y: 4)
        )
        .contentShape(Rectangle())
        .onTapGesture { if mergeSource == nil { onOpen() } }
        .help("Click to see every photo for this person")
    }
}

// MARK: - Suggested Merge Mini Card

struct SuggestedMergeMini: View {
    let identity: PersonRecord

    var body: some View {
        VStack(spacing: 4) {
            if let data = identity.representativeFaceCropData, let img = NSImage(data: data) {
                Image(nsImage: img).resizable().scaledToFill()
                    .frame(width: 44, height: 44).clipShape(Circle())
            } else if let url = identity.sampleFileURLs.first {
                ThumbnailView(url: url).frame(width: 44, height: 44).clipShape(Circle())
            } else {
                Circle().fill(.gray.opacity(0.3)).frame(width: 44, height: 44)
                    .overlay(Image(systemName: "person.fill").foregroundStyle(.gray))
            }
            Text(identity.name ?? "Unknown")
                .font(.system(size: 9)).lineLimit(1)
                .frame(width: 56)
        }
    }
}
