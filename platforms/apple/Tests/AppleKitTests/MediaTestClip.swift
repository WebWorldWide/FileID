import AVFoundation
import CoreMedia
import CoreVideo
import Foundation

/// Synthetic H.264/AAC clips for the media adapter tests: a moving bright bar on
/// a dark frame (so frame signatures change) and a sine tone.
enum MediaTestClip {
    struct Failure: Error { let step: String }

    static func write(to url: URL, width: Int = 96, height: Int = 54, seconds: Double = 2, fps: Int32 = 24,
                      rotation: Double = 0, audio: Bool = true, fileType: AVFileType = .mp4) async throws {
        let writer = try AVAssetWriter(outputURL: url, fileType: fileType)
        let video = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: width, AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [AVVideoMaxKeyFrameIntervalKey: Int(fps)],
        ])
        video.expectsMediaDataInRealTime = false
        video.transform = CGAffineTransform(rotationAngle: rotation * .pi / 180)
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: video, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: width, kCVPixelBufferHeightKey as String: height,
        ])
        guard writer.canAdd(video) else { throw Failure(step: "video input") }
        writer.add(video)
        var sound: AVAssetWriterInput?
        if audio {
            let input = AVAssetWriterInput(mediaType: .audio, outputSettings: [
                AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: 44_100, AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 64_000,
            ])
            input.expectsMediaDataInRealTime = false
            guard writer.canAdd(input) else { throw Failure(step: "audio input") }
            writer.add(input)
            sound = input
        }
        guard writer.startWriting() else { throw writer.error ?? Failure(step: "start") }
        writer.startSession(atSourceTime: .zero)
        let frames = Int(seconds * Double(fps))
        let rate = 44_100
        let feeder = Feeder(width: width, height: height, frames: frames, fps: fps, adaptor: adaptor,
                            totalSamples: Int(seconds * Double(rate)), rate: rate, video: video, sound: sound)
        try feeder.run()
        await writer.finishWriting()
        guard writer.status == .completed else { throw writer.error ?? Failure(step: "finish") }
    }

    /// Each input pulls on its own queue so AVAssetWriter can interleave them.
    private final class Feeder: @unchecked Sendable {
        let width: Int, height: Int, frames: Int, fps: Int32, totalSamples: Int, rate: Int
        let adaptor: AVAssetWriterInputPixelBufferAdaptor
        let video: AVAssetWriterInput
        let sound: AVAssetWriterInput?
        var frame = 0
        var written = 0
        var failure: Error?

        init(width: Int, height: Int, frames: Int, fps: Int32, adaptor: AVAssetWriterInputPixelBufferAdaptor,
             totalSamples: Int, rate: Int, video: AVAssetWriterInput, sound: AVAssetWriterInput?) {
            (self.width, self.height, self.frames, self.fps) = (width, height, frames, fps)
            (self.adaptor, self.totalSamples, self.rate, self.video, self.sound) = (adaptor, totalSamples, rate, video, sound)
        }

        func run() throws {
            let group = DispatchGroup()
            pull(video, queue: DispatchQueue(label: "clip.video"), group: group) { self.nextVideo() }
            if let sound { pull(sound, queue: DispatchQueue(label: "clip.audio"), group: group) { self.nextAudio(into: sound) } }
            group.wait()
            if let failure { throw failure }
        }

        private func pull(_ input: AVAssetWriterInput, queue: DispatchQueue, group: DispatchGroup, next: @escaping () -> Bool) {
            group.enter()
            var finished = false
            input.requestMediaDataWhenReady(on: queue) {
                while input.isReadyForMoreMediaData, !finished {
                    if !next() {
                        finished = true
                        input.markAsFinished()
                        group.leave()
                    }
                }
            }
        }

        private func nextVideo() -> Bool {
            guard frame < frames, failure == nil else { return false }
            guard let pool = adaptor.pixelBufferPool else { failure = Failure(step: "pixel pool"); return false }
            var buffer: CVPixelBuffer?
            CVPixelBufferPoolCreatePixelBuffer(nil, pool, &buffer)
            guard let buffer else { failure = Failure(step: "pixel buffer"); return false }
            MediaTestClip.paint(buffer, width: width, height: height, frame: frame, frames: frames)
            guard adaptor.append(buffer, withPresentationTime: CMTime(value: CMTimeValue(frame), timescale: fps)) else {
                failure = Failure(step: "append video"); return false
            }
            frame += 1
            return true
        }

        private func nextAudio(into input: AVAssetWriterInput) -> Bool {
            guard written < totalSamples, failure == nil else { return false }
            let count = min(1024, totalSamples - written)
            do {
                guard input.append(try MediaTestClip.tone(start: written, count: count, rate: rate)) else { failure = Failure(step: "append audio"); return false }
            } catch { failure = error; return false }
            written += count
            return true
        }
    }

    fileprivate static func paint(_ buffer: CVPixelBuffer, width: Int, height: Int, frame: Int, frames: Int) {
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        guard let base = CVPixelBufferGetBaseAddress(buffer) else { return }
        let row = CVPixelBufferGetBytesPerRow(buffer)
        let bar = max(4, width / 6)
        let left = (width - bar) * frame / max(1, frames - 1)
        let pixels = base.assumingMemoryBound(to: UInt8.self)
        for y in 0..<height {
            for x in 0..<width {
                let offset = y * row + x * 4
                let lit = x >= left && x < left + bar
                let top = y < height / 3
                pixels[offset] = lit ? 240 : 30
                pixels[offset + 1] = lit ? 240 : (top ? 120 : 30)
                pixels[offset + 2] = lit ? 240 : (top ? 30 : 60)
                pixels[offset + 3] = 255
            }
        }
    }

    fileprivate static func tone(start: Int, count: Int, rate: Int) throws -> CMSampleBuffer {
        var samples = [Int16](repeating: 0, count: count)
        for index in 0..<count {
            samples[index] = Int16(sin(Double(start + index) * 2 * .pi * 440 / Double(rate)) * 8_000)
        }
        var description = AudioStreamBasicDescription(
            mSampleRate: Double(rate), mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kLinearPCMFormatFlagIsSignedInteger | kLinearPCMFormatFlagIsPacked,
            mBytesPerPacket: 2, mFramesPerPacket: 1, mBytesPerFrame: 2, mChannelsPerFrame: 1, mBitsPerChannel: 16, mReserved: 0)
        var format: CMAudioFormatDescription?
        guard CMAudioFormatDescriptionCreate(allocator: nil, asbd: &description, layoutSize: 0, layout: nil, magicCookieSize: 0,
                                             magicCookie: nil, extensions: nil, formatDescriptionOut: &format) == noErr,
              let format else { throw Failure(step: "audio format") }
        var block: CMBlockBuffer?
        let bytes = count * 2
        guard CMBlockBufferCreateWithMemoryBlock(allocator: nil, memoryBlock: nil, blockLength: bytes, blockAllocator: nil,
                                                 customBlockSource: nil, offsetToData: 0, dataLength: bytes, flags: 0,
                                                 blockBufferOut: &block) == noErr, let block else { throw Failure(step: "audio block") }
        try samples.withUnsafeBytes { raw in
            guard CMBlockBufferReplaceDataBytes(with: raw.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: bytes) == noErr else {
                throw Failure(step: "audio bytes")
            }
        }
        var sample: CMSampleBuffer?
        guard CMAudioSampleBufferCreateReadyWithPacketDescriptions(
            allocator: nil, dataBuffer: block, formatDescription: format, sampleCount: count,
            presentationTimeStamp: CMTime(value: CMTimeValue(start), timescale: CMTimeScale(rate)),
            packetDescriptions: nil, sampleBufferOut: &sample) == noErr, let sample else { throw Failure(step: "audio sample") }
        return sample
    }
}
