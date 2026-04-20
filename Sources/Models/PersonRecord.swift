import Foundation
import SwiftData
import Vision

@Model
final class PersonRecord {
    @Attribute(.unique) var id: UUID
    var name: String?
    var representativeFaceCropData: Data? // JPEG data of the best face crop
    
    // Feature prints stored as Data (serialized VNFeaturePrintObservation)
    var featurePrintsData: [Data] = []
    
    // How many times this person has been detected across all scanned files
    var faceCount: Int = 1
    
    // URLs of files containing this person (for sample thumbnails in PeopleView)
    var sampleFileURLs: [URL] = []
    
    init(id: UUID = UUID(), name: String? = nil, representativeFaceCropData: Data? = nil) {
        self.id = id
        self.name = name
        self.representativeFaceCropData = representativeFaceCropData
    }
}
