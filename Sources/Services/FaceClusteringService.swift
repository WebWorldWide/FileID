import Foundation
import Vision
import CoreImage

struct PersonIdentity: Identifiable {
    let id = UUID()
    var name: String? // Nil if un-named ("Unknown Person")
    var featurePrints: [VNFeaturePrintObservation]
    var representativeFaceCrop: CGImage // To show in the UI
}

actor FaceClusteringService {
    static let shared = FaceClusteringService()
    
    private var knownIdentities: [PersonIdentity] = []
    
    // threshold for VNFeaturePrintObservation
    let distanceThreshold: Float = 20.0 
    
    func cluster(facePrint: VNFeaturePrintObservation, crop: CGImage) -> PersonIdentity {
        var minDistance = Float.infinity
        var closestIdentityIndex: Int?
        
        for (index, identity) in knownIdentities.enumerated() {
            // PhD Optimization: Compare ONLY against the first/representative print to reduce complexity from O(N*M) to O(N)
            guard let knownPrint = identity.featurePrints.first else { continue }
            var distance: Float = 0
            try? knownPrint.computeDistance(&distance, to: facePrint)
            if distance < minDistance {
                minDistance = distance
                closestIdentityIndex = index
            }
        }
        
        if minDistance < distanceThreshold, let index = closestIdentityIndex {
            // It's a match!
            knownIdentities[index].featurePrints.append(facePrint)
            return knownIdentities[index]
        } else {
            // Unknown person
            let newIdentity = PersonIdentity(name: nil, featurePrints: [facePrint], representativeFaceCrop: crop)
            knownIdentities.append(newIdentity)
            return newIdentity
        }
    }
    
    func allIdentities() -> [PersonIdentity] {
        return knownIdentities
    }
    
    func updateIdentityName(id: UUID, newName: String) {
        if let index = knownIdentities.firstIndex(where: { $0.id == id }) {
            knownIdentities[index].name = newName
        }
    }
    
    func mergeIdentities(sourceId: UUID, targetId: UUID) {
        guard sourceId != targetId,
              let sourceIndex = knownIdentities.firstIndex(where: { $0.id == sourceId }),
              let targetIndex = knownIdentities.firstIndex(where: { $0.id == targetId }) else { return }
        
        let source = knownIdentities.remove(at: sourceIndex)
        let actualTargetIndex = knownIdentities.firstIndex(where: { $0.id == targetId })! // re-fetch after remove
        
        knownIdentities[actualTargetIndex].featurePrints.append(contentsOf: source.featurePrints)
        // Name stays the target's name
    }
}
