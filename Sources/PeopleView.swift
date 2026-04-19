import SwiftUI

struct PeopleView: View {
    @ObservedObject var viewModel: AppViewModel
    @State private var identities: [PersonIdentity] = []
    @State private var selectedIdentity: PersonIdentity?
    @State private var namingText: String = ""
    @State private var isNaming = false
    @State private var mergeSource: PersonIdentity?
    
    // Auto-refresh timer
    let timer = Timer.publish(every: 3.0, on: .main, in: .common).autoconnect()
    
    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack {
                Image(systemName: "person.2.fill")
                    .font(.largeTitle)
                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                Text("People & Identities")
                    .font(.title.bold())
                Spacer()
                Button("Refresh") {
                    Task { await loadIdentities() }
                }
                .buttonStyle(.bordered)
            }
            .padding(.horizontal)
            .padding(.top, 20)
            
            if identities.isEmpty {
                VStack {
                    Spacer()
                    Image(systemName: "face.dashed")
                        .font(.system(size: 60))
                        .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                    Text("No People Detected Yet")
                        .font(.headline)
                        .foregroundStyle(.secondary)
                    Text("The AI is scanning your files for familiar faces. They will appear here automatically.")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                        .multilineTextAlignment(.center)
                        .padding()
                    Spacer()
                }
                .frame(maxWidth: .infinity)
            } else {
                ScrollView {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 160), spacing: 20)], spacing: 30) {
                        ForEach(identities) { identity in
                            VStack {
                                ZStack {
                                    Image(nsImage: NSImage(cgImage: identity.representativeFaceCrop, size: .zero))
                                        .resizable()
                                        .scaledToFill()
                                        .frame(width: 120, height: 120)
                                        .clipShape(Circle())
                                        .overlay(Circle().stroke(mergeSource?.id == identity.id ? Color.blue : Color(red: 1.0, green: 0.8, blue: 0.0), lineWidth: 3))
                                        .shadow(radius: 10)
                                    
                                    if mergeSource != nil && mergeSource?.id != identity.id {
                                        Button {
                                            merge(source: mergeSource!, into: identity)
                                        } label: {
                                            Image(systemName: "plus.circle.fill")
                                                .font(.title)
                                                .foregroundStyle(.blue)
                                        }
                                        .buttonStyle(.plain)
                                        .offset(x: 45, y: -45)
                                    }
                                }
                                
                                Text(identity.name ?? "Unknown Person")
                                    .font(.headline)
                                    .lineLimit(1)
                                
                                HStack {
                                    Button("Name") {
                                        selectedIdentity = identity
                                        namingText = identity.name ?? ""
                                        isNaming = true
                                    }
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
                            .padding()
                            .background(RoundedRectangle(cornerRadius: 20).fill(Color.white.opacity(0.05)))
                        }
                    }
                    .padding()
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.opacity(0.3))
        .onAppear {
            Task { await loadIdentities() }
        }
        .onReceive(timer) { _ in
            if viewModel.isProcessing {
                Task { await loadIdentities() }
            }
        }
        .sheet(isPresented: $isNaming) {
            VStack(spacing: 20) {
                Text("Identify Person")
                    .font(.headline)
                
                TextField("Name", text: $namingText)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 250)
                
                HStack {
                    Button("Cancel") { isNaming = false }
                    Button("Save") {
                        if let id = selectedIdentity {
                            Task {
                                await FaceClusteringService.shared.updateIdentityName(id: id.id, newName: namingText)
                                await loadIdentities()
                                isNaming = false
                            }
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                }
            }
            .padding()
            .frame(width: 300, height: 180)
        }
    }
    
    private func loadIdentities() async {
        let items = await FaceClusteringService.shared.allIdentities()
        await MainActor.run {
            self.identities = items
        }
    }
    
    private func merge(source: PersonIdentity, into target: PersonIdentity) {
        Task {
            await FaceClusteringService.shared.mergeIdentities(sourceId: source.id, targetId: target.id)
            await loadIdentities()
            await MainActor.run { mergeSource = nil }
        }
    }
}
