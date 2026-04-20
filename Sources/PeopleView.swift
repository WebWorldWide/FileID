import SwiftUI
import SwiftData

struct PeopleView: View {
    @ObservedObject var viewModel: AppViewModel
    @Environment(\.modelContext) private var context
    @Query(sort: [SortDescriptor(\PersonRecord.faceCount, order: .reverse)]) private var identities: [PersonRecord]
    
    @State private var selectedIdentity: PersonRecord?
    @State private var namingText: String = ""
    @State private var isNaming = false
    @State private var mergeSource: PersonRecord?
    @State private var searchText: String = ""
    @State private var sortOption: SortOption = .byCount
    
    enum SortOption: String, CaseIterable {
        case byCount = "Most Photos"
        case byName = "Name"
        case byRecent = "Recently Added"
    }
    
    var filteredIdentities: [PersonRecord] {
        var result = identities
        
        // Filter by search
        if !searchText.isEmpty {
            result = result.filter { identity in
                (identity.name ?? "Unknown").lowercased().contains(searchText.lowercased())
            }
        }
        
        // Sort
        switch sortOption {
        case .byCount:
            result.sort { $0.faceCount > $1.faceCount }
        case .byName:
            result.sort { ($0.name ?? "zzz") < ($1.name ?? "zzz") }
        case .byRecent:
            break // Already sorted by insertion order via @Query
        }
        
        return result
    }
    
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header
            HStack {
                Image(systemName: "person.2.fill")
                    .font(.largeTitle)
                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
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
                .frame(width: 280)
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
                }
            }
            .padding(8)
            .background(RoundedRectangle(cornerRadius: 8).fill(Color.white.opacity(0.1)))
            .padding(.horizontal)
            .padding(.bottom, 12)
            
            if filteredIdentities.isEmpty {
                VStack {
                    Spacer()
                    Image(systemName: "face.dashed")
                        .font(.system(size: 60))
                        .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
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
            } else {
                ScrollView {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 200), spacing: 20)], spacing: 24) {
                        ForEach(filteredIdentities) { identity in
                            PersonCard(
                                identity: identity,
                                mergeSource: $mergeSource,
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
                    Button("Save") {
                        if let identity = selectedIdentity {
                            identity.name = namingText
                            try? context.save()
                            isNaming = false
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                }
            }
            .padding()
            .frame(width: 320, height: 260)
        }
    }
    
    private func merge(source: PersonRecord, into target: PersonRecord) {
        Task {
            try? await FaceClusteringService.shared.merge(sourceID: source.id, targetID: target.id, context: context)
            await MainActor.run { mergeSource = nil }
        }
    }
}

// MARK: - Person Card

struct PersonCard: View {
    let identity: PersonRecord
    @Binding var mergeSource: PersonRecord?
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
                            mergeSource?.id == identity.id ? Color.blue : Color(red: 1.0, green: 0.8, blue: 0.0),
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
                    .background(Capsule().fill(Color(red: 1.0, green: 0.8, blue: 0.0)))
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
                    .offset(x: 6, y: -40)
                }
            }
            
            // Name
            Text(identity.name ?? "Unknown Person")
                .font(.system(size: 14, weight: .semibold))
                .lineLimit(1)
            
            // Photo count
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
            }
        }
        .padding(16)
        .background(
            RoundedRectangle(cornerRadius: 20)
                .fill(.ultraThinMaterial)
                .shadow(color: .black.opacity(0.15), radius: 8, y: 4)
        )
    }
}
