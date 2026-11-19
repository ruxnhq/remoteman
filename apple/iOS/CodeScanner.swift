// Scanning a computer's pairing code with this device's camera. The camera is asked for only
// when the person taps to scan, never when the screen opens.
import AVFoundation
import SwiftUI

struct CodeScanner: UIViewRepresentable {
    /// Called once, on the main thread, with the first remoteman pairing link seen.
    let found: (PairLink) -> Void

    func makeUIView(context: Context) -> ScannerView {
        let v = ScannerView()
        v.found = found
        v.start()
        return v
    }

    func updateUIView(_ v: ScannerView, context: Context) {}
    static func dismantleUIView(_ v: ScannerView, coordinator: ()) { v.stop() }

    final class ScannerView: UIView, AVCaptureMetadataOutputObjectsDelegate {
        var found: ((PairLink) -> Void)?
        private let session = AVCaptureSession()
        private let queue = DispatchQueue(label: "remoteman.scanner")
        private var done = false
        override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
        private var preview: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer }

        func start() {
            backgroundColor = .black
            preview.videoGravity = .resizeAspectFill
            guard let camera = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back) ?? AVCaptureDevice.default(for: .video),
                let input = try? AVCaptureDeviceInput(device: camera), session.canAddInput(input)
            else { return }
            session.addInput(input)
            let output = AVCaptureMetadataOutput()
            guard session.canAddOutput(output) else { return }
            session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            if output.availableMetadataObjectTypes.contains(.qr) { output.metadataObjectTypes = [.qr] }
            preview.session = session
            queue.async { [session] in session.startRunning() }
        }

        func stop() { queue.async { [session] in session.stopRunning() } }

        override func layoutSubviews() {
            super.layoutSubviews()
            // The picture follows the way the device is held.
            if let c = preview.connection, let scene = window?.windowScene {
                let angle: CGFloat =
                    switch scene.interfaceOrientation {
                    case .landscapeLeft: 180
                    case .landscapeRight: 0
                    case .portraitUpsideDown: 270
                    default: 90
                    }
                if c.isVideoRotationAngleSupported(angle) { c.videoRotationAngle = angle }
            }
        }

        func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from connection: AVCaptureConnection) {
            guard !done else { return }
            for o in objects {
                guard let text = (o as? AVMetadataMachineReadableCodeObject)?.stringValue, let link = PairLink(text) else { continue }
                done = true
                stop()
                found?(link)
                return
            }
        }
    }
}
