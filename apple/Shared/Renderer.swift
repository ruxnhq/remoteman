// Metal presentation: the decoder's 10-bit biplanar pixel buffers become two textures with no
// copy, a shader turns BT.709 limited-range YCbCr into RGB, and the newest frame is drawn at
// each display refresh.
import CoreVideo
import Metal
import QuartzCore

private let shaderSource = """
#include <metal_stdlib>
using namespace metal;

struct VOut { float4 pos [[position]]; float2 uv; };

struct Uniforms { float2 scale; float2 offset; };

vertex VOut rm_vertex(uint vid [[vertex_id]], constant Uniforms &u [[buffer(0)]]) {
    float2 p[4] = { float2(-1, -1), float2(1, -1), float2(-1, 1), float2(1, 1) };
    float2 t[4] = { float2(0, 1), float2(1, 1), float2(0, 0), float2(1, 0) };
    VOut o;
    o.pos = float4(p[vid] * u.scale + u.offset, 0, 1);
    o.uv = t[vid];
    return o;
}

// 10 bits sit in the top of each 16-bit sample, so a sample reads as value/1023 (near enough).
fragment float4 rm_fragment(VOut in [[stage_in]], texture2d<float> luma [[texture(0)]],
                            texture2d<float> chroma [[texture(1)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float y = (luma.sample(s, in.uv).r - 64.0 / 1023.0) * (1023.0 / 876.0);
    float2 c = (chroma.sample(s, in.uv).rg - 512.0 / 1023.0) * (1023.0 / 896.0);
    float3 rgb = float3(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x);
    return float4(saturate(rgb), 1.0);
}

struct Light { float sdrWhite; float headroom; };

// HDR: BT.2020 limited-range YCbCr with the PQ curve becomes linear BT.2020 light in which 1.0
// is the host's SDR white, so that its ordinary white lands on this screen's and what is brighter
// goes into the screen's headroom. What is brighter than the screen can show is eased towards
// the top instead of cut off; nothing at or under SDR white is touched.
fragment float4 rm_fragment_hdr(VOut in [[stage_in]], texture2d<float> luma [[texture(0)]],
                                texture2d<float> chroma [[texture(1)]], constant Light &l [[buffer(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float y = (luma.sample(s, in.uv).r - 64.0 / 1023.0) * (1023.0 / 876.0);
    float2 c = (chroma.sample(s, in.uv).rg - 512.0 / 1023.0) * (1023.0 / 896.0);
    float3 e = saturate(float3(y + 1.4746 * c.y, y - 0.16455 * c.x - 0.57135 * c.y, y + 1.8814 * c.x));
    float3 p = pow(e, float3(1.0 / 78.84375));
    float3 nits = 10000.0 * pow(max(p - 0.8359375, 0.0) / (18.8515625 - 18.6875 * p), float3(1.0 / 0.1593017578125));
    // BT.2020 to Display P3 primaries (linear light): an extended-range P3 layer is what iOS draws
    // with headroom (it doesn't take an extended BT.2020 one), and macOS takes it too.
    const float3x3 toP3 = float3x3(float3(1.3435783, -0.0652975, 0.0028218),
                                   float3(-0.2821797, 1.0757879, -0.0195985),
                                   float3(-0.0613986, -0.0104905, 1.0167767));
    float3 v = toP3 * (nits / l.sdrWhite);
    float m = max(v.r, max(v.g, v.b));
    float room = max(l.headroom - 1.0, 0.001);
    if (m > 1.0) { v *= (1.0 + room * (1.0 - exp(-(m - 1.0) / room))) / m; }
    return float4(v, 1.0);
}
"""

final class VideoRenderer {
    struct Frame {
        let pixelBuffer: CVPixelBuffer
        let number: UInt32
        /// The frame's age when it was handed over by the core, and when that was (uptime ns).
        let ageUs: Int64
        let receivedNs: UInt64
    }

    let device: MTLDevice
    let layer: CAMetalLayer
    private let queue: MTLCommandQueue
    private let library: MTLLibrary
    private var pipelines: [String: MTLRenderPipelineState] = [:]
    private var reportedNoDrawable = false, reportedNoPipeline = false
    /// The stream is HDR (BT.2020, PQ): drawn into a float layer in which 1.0 is SDR white.
    private(set) var hdr = false
    /// Where SDR white sits in the HDR signal, in nits (the host says; 203 if it doesn't).
    var sdrWhite: Float = 203
    /// How many times brighter than SDR white the screen shows right now. Asked at each frame.
    var headroom: () -> Float = { 1 }
    private let sdrFormat: MTLPixelFormat

    /// Main thread: follow the stream between SDR and HDR.
    func setHDR(_ on: Bool) {
        guard on != hdr else { return }
        hdr = on
        if #available(iOS 16.0, macOS 10.11, *) { layer.wantsExtendedDynamicRangeContent = on }
        layer.pixelFormat = on ? .rgba16Float : sdrFormat
        layer.colorspace = CGColorSpace(name: on ? CGColorSpace.extendedLinearDisplayP3 : CGColorSpace.sRGB)
        setNeedsRedraw()
    }

    /// For checking HDR: the picture's value at a point (fractions from its top left), in the
    /// viewer's linear light where 1.0 is SDR white, with nothing eased for the screen.
    func probe(x: Double, y: Double) -> SIMD4<Float>? {
        lock.lock()
        let frame = pending ?? current
        lock.unlock()
        guard let pb = frame?.pixelBuffer, let cb = queue.makeCommandBuffer() else { return nil }
        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
        let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba16Float, width: w, height: h, mipmapped: false)
        d.usage = [.renderTarget, .shaderRead]
        d.storageMode = .shared
        guard let target = device.makeTexture(descriptor: d) else { return nil }
        let keep = encode(pb, into: target, format: .rgba16Float, commandBuffer: cb, zoomed: false, room: 1000)
        cb.commit()
        cb.waitUntilCompleted()
        _ = keep
        var px = [Float16](repeating: 0, count: 4)
        let at = MTLRegionMake2D(min(w - 1, max(0, Int(x * Double(w)))), min(h - 1, max(0, Int(y * Double(h)))), 1, 1)
        target.getBytes(&px, bytesPerRow: 8, from: at, mipmapLevel: 0)
        return SIMD4(Float(px[0]), Float(px[1]), Float(px[2]), Float(px[3]))
    }
    private var cache: CVMetalTextureCache!
    private let lock = NSLock()
    /// The size to draw at, in pixels: the view's. Set and used on the main thread.
    var drawableSize = CGSize.zero
    private var pending: Frame?
    private var current: Frame?
    private var needsRedraw = false

    /// Frames replaced before they were ever drawn, and frames drawn. Read under `lock` via `counters()`.
    private var superseded = 0
    private var presented = 0
    private var lastAgeAtPresentUs: Int64 = -1
    /// Every frame shown: when (uptime ns) and how old it was (µs, -1 unknown). For run summaries.
    private var shownLog: [(UInt64, Int64)] = []
    /// For each frame, ms from its arrival here to the moment the screen showed it.
    private var onScreenLog: [Double] = []

    /// Each frame is drawn and presented the moment it is handed over, instead of at the next tick of
    /// the display link: it reaches the screen at the screen's next refresh, up to a whole tick
    /// sooner, and frames that come unevenly are shown unevenly as they came. Set before frames come.
    /// Off by default: on the iPad Pro (120 Hz) a frame presented as it came reached the screen no
    /// sooner (median 15.0 against 14.3 ms) and the slowest 5% a frame later (30 against 20 ms):
    /// presented at a random moment it often misses the compositor's deadline for the coming
    /// refresh, where the display link's tick comes just after one. `--present arrival` to compare.
    static var onArrival = false
    private let presentQueue = DispatchQueue(label: "remoteman.present", qos: .userInteractive)
    /// The size the layer is drawn at, set on the main thread only (CAMetalLayer's own is read off it).
    private var layerSize = CGSize.zero

    func takeOnScreenLog() -> [Double] {
        lock.lock()
        defer { lock.unlock() }
        let l = onScreenLog
        onScreenLog = []
        return l
    }

    /// Main thread: keep the layer's size and, when frames are drawn as they come, draw what needs
    /// drawing again (a zoom, a new size, the end of a picture). Call at each display refresh.
    func tick() {
        guard Self.onArrival else { return draw() }
        lock.lock()
        let frame = pending ?? current
        let redraw = needsRedraw
        lock.unlock()
        let size = frame == nil ? CGSize(width: 16, height: 16) : drawableSize
        if size.width >= 1, size.height >= 1, layer.drawableSize != size {
            layer.drawableSize = size
            setNeedsRedraw()
        }
        layerSize = layer.drawableSize
        if redraw { presentQueue.async { [weak self] in self?.draw(sized: false) } }
    }

    func takeShownLog() -> [(UInt64, Int64)] {
        lock.lock()
        defer { lock.unlock() }
        let l = shownLog
        shownLog = []
        return l
    }

    init?() {
        guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue() else { return nil }
        self.device = device
        self.queue = queue
        do { library = try device.makeLibrary(source: shaderSource, options: nil) } catch {
            NSLog("remoteman: shader: \(error)")
            return nil
        }
        layer = CAMetalLayer()
        layer.device = device
        #if os(macOS)
            sdrFormat = .bgr10a2Unorm
        #else
            sdrFormat = .bgra8Unorm
        #endif
        layer.pixelFormat = sdrFormat
        layer.framebufferOnly = true
        layer.colorspace = CGColorSpace(name: CGColorSpace.sRGB)
        layer.isOpaque = true
        CVMetalTextureCacheCreate(nil, nil, device, nil, &cache)
    }

    private func pipeline(_ format: MTLPixelFormat, hdr: Bool) -> MTLRenderPipelineState? {
        let key = "\(format.rawValue)-\(hdr)"
        if let p = pipelines[key] { return p }
        let d = MTLRenderPipelineDescriptor()
        d.vertexFunction = library.makeFunction(name: "rm_vertex")
        d.fragmentFunction = library.makeFunction(name: hdr ? "rm_fragment_hdr" : "rm_fragment")
        d.colorAttachments[0].pixelFormat = format
        let p = try? device.makeRenderPipelineState(descriptor: d)
        pipelines[key] = p
        return p
    }

    /// Hand over a decoded frame. Any thread. The newest frame wins.
    func submit(_ f: Frame) {
        lock.lock()
        if pending != nil { superseded += 1 }
        pending = f
        lock.unlock()
        // One draw waiting at a time: frames that come while it waits only replace what it will
        // draw, so that a burst never queues up drawables (newest wins, as at the tick).
        if Self.onArrival {
            lock.lock()
            let start = !drawQueued
            drawQueued = true
            lock.unlock()
            if start {
                presentQueue.async { [weak self] in
                    guard let self else { return }
                    self.lock.lock()
                    self.drawQueued = false
                    self.lock.unlock()
                    self.draw(sized: false)
                }
            }
        }
    }
    private var drawQueued = false

    func clear() {
        lock.lock()
        pending = nil
        current = nil
        needsRedraw = true
        lock.unlock()
        // The next session starts with the whole picture in view.
        zoom = 1
        zoomOrigin = .zero
    }

    func setNeedsRedraw() {
        lock.lock()
        needsRedraw = true
        lock.unlock()
    }

    func counters() -> (presented: Int, superseded: Int, ageAtPresentUs: Int64) {
        lock.lock()
        defer { lock.unlock() }
        return (presented, superseded, lastAgeAtPresentUs)
    }

    /// The size of the picture being shown, in pixels, if any.
    var videoSize: CGSize? {
        lock.lock()
        defer { lock.unlock() }
        guard let pb = (pending ?? current)?.pixelBuffer else { return nil }
        return CGSize(width: CVPixelBufferGetWidth(pb), height: CVPixelBufferGetHeight(pb))
    }

    /// The view's own magnification of the picture (1: the whole picture fits), and where the
    /// magnified picture's top left sits, as fractions of the view from its top left. Set on
    /// the main thread (a pinch on iPhone and iPad); input is mapped through the same rectangle.
    var zoom: CGFloat = 1 { didSet { setNeedsRedraw() } }
    var zoomOrigin = CGPoint.zero { didSet { setNeedsRedraw() } }

    /// Where the picture sits inside a view of `bounds` (aspect fit, centred; magnified if the
    /// view is zoomed), in the view's units.
    func videoRect(in bounds: CGSize, zoomed: Bool = true) -> CGRect {
        guard let v = videoSize, v.width > 0, v.height > 0, bounds.width > 0, bounds.height > 0 else {
            return CGRect(origin: .zero, size: bounds)
        }
        let s = min(bounds.width / v.width, bounds.height / v.height)
        let z = zoomed ? max(1, zoom) : 1
        let w = v.width * s * z, h = v.height * s * z
        // A picture larger than the view covers it edge to edge; a smaller one is centred.
        func place(_ origin: CGFloat, _ size: CGFloat, _ room: CGFloat) -> CGFloat {
            size <= room ? (room - size) / 2 : min(0, max(room - size, origin))
        }
        return CGRect(
            x: place(z > 1 ? zoomOrigin.x * bounds.width : 0, w, bounds.width), y: place(z > 1 ? zoomOrigin.y * bounds.height : 0, h, bounds.height),
            width: w, height: h)
    }

    private func textures(_ pb: CVPixelBuffer) -> (MTLTexture, MTLTexture, CVMetalTexture, CVMetalTexture)? {
        guard CVPixelBufferGetPlaneCount(pb) == 2 else { return nil }
        var y: CVMetalTexture?
        var c: CVMetalTexture?
        CVMetalTextureCacheCreateTextureFromImage(
            nil, cache, pb, nil, .r16Unorm, CVPixelBufferGetWidthOfPlane(pb, 0), CVPixelBufferGetHeightOfPlane(pb, 0), 0, &y)
        CVMetalTextureCacheCreateTextureFromImage(
            nil, cache, pb, nil, .rg16Unorm, CVPixelBufferGetWidthOfPlane(pb, 1), CVPixelBufferGetHeightOfPlane(pb, 1), 1, &c)
        guard let y, let c, let ty = CVMetalTextureGetTexture(y), let tc = CVMetalTextureGetTexture(c) else { return nil }
        return (ty, tc, y, c)
    }

    private func encode(
        _ pb: CVPixelBuffer?, into target: MTLTexture, format: MTLPixelFormat, commandBuffer: MTLCommandBuffer, zoomed: Bool = true,
        room: Float? = nil
    ) -> [Any] {
        // An HDR picture into a float target keeps its light; into any other (a screenshot) it is
        // eased down to SDR white.
        let float = format == .rgba16Float
        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = target
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)
        pass.colorAttachments[0].storeAction = .store
        guard let enc = commandBuffer.makeRenderCommandEncoder(descriptor: pass) else { return [] }
        var keep: [Any] = []
        if pb != nil, pipeline(format, hdr: hdr) == nil, !reportedNoPipeline {
            reportedNoPipeline = true
            print("remoteman: no pipeline for format \(format.rawValue), HDR \(hdr)")
        }
        if let pb, let (ty, tc, ky, kc) = textures(pb), let p = pipeline(format, hdr: hdr) {
            let tw = Double(target.width), th = Double(target.height)
            let vw = Double(CVPixelBufferGetWidth(pb)), vh = Double(CVPixelBufferGetHeight(pb))
            // The same rectangle input is mapped through, here in the target's pixels.
            _ = (vw, vh)
            let r = videoRect(in: CGSize(width: tw, height: th), zoomed: zoomed)
            var uniforms = SIMD4<Float>(
                Float(r.width / tw), Float(r.height / th), Float((r.midX / tw) * 2 - 1), Float(1 - (r.midY / th) * 2))
            enc.setRenderPipelineState(p)
            enc.setVertexBytes(&uniforms, length: MemoryLayout<SIMD4<Float>>.size, index: 0)
            enc.setFragmentTexture(ty, index: 0)
            enc.setFragmentTexture(tc, index: 1)
            if hdr {
                var light = SIMD2<Float>(max(sdrWhite, 1), float ? (room ?? max(headroom(), 1)) : 1)
                enc.setFragmentBytes(&light, length: MemoryLayout<SIMD2<Float>>.size, index: 0)
            }
            enc.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
            keep = [ky, kc, pb]
        }
        enc.endEncoding()
        return keep
    }

    /// Draw the newest frame if there is one that hasn't been shown. Call at each display refresh.
    func draw(sized: Bool = true) {
        lock.lock()
        let fresh = pending
        pending = nil
        if let fresh { current = fresh }
        let frame = current
        let redraw = needsRedraw
        needsRedraw = false
        lock.unlock()
        guard fresh != nil || redraw else { return }
        // With no picture the layer is black whatever its size: a few pixels then, so that the
        // full-size buffers behind the view go back until there is a picture again.
        if sized {
            let size = frame == nil ? CGSize(width: 16, height: 16) : drawableSize
            if size.width >= 1, size.height >= 1, layer.drawableSize != size { layer.drawableSize = size }
        }
        guard let drawable = layer.nextDrawable(), let cb = queue.makeCommandBuffer() else {
            if !reportedNoDrawable { reportedNoDrawable = true; print("remoteman: the layer gave no drawable (format \(layer.pixelFormat.rawValue))") }
            return
        }
        if let fresh {
            // When the screen really showed it, against when it came.
            let came = fresh.receivedNs
            drawable.addPresentedHandler { [weak self] d in
                guard d.presentedTime > 0, let self else { return }
                let ms = (d.presentedTime * 1e9 - Double(came)) / 1e6
                self.lock.lock()
                if self.onScreenLog.count < 200_000 { self.onScreenLog.append(ms) }
                self.lock.unlock()
            }
        }
        let keep = encode(frame?.pixelBuffer, into: drawable.texture, format: layer.pixelFormat, commandBuffer: cb)
        cb.present(drawable)
        // With no picture left to show, the decoded pictures the texture cache still holds go
        // back once this (black) frame is done.
        let cache = frame == nil ? self.cache : nil
        cb.addCompletedHandler { _ in
            _ = keep
            if let cache { CVMetalTextureCacheFlush(cache, 0) }
        }
        cb.commit()
        if let fresh {
            let now = DispatchTime.now().uptimeNanoseconds
            lock.lock()
            presented += 1
            lastAgeAtPresentUs = fresh.ageUs < 0 ? -1 : fresh.ageUs + Int64((now - fresh.receivedNs) / 1000)
            if shownLog.count < 200_000 { shownLog.append((now, lastAgeAtPresentUs)) }
            lock.unlock()
        }
    }

    /// The frame on screen, drawn through the same shader into an 8-bit image (for screenshots).
    func snapshot() -> CGImage? {
        lock.lock()
        let frame = pending ?? current
        lock.unlock()
        guard let pb = frame?.pixelBuffer else { return nil }
        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
        let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: w, height: h, mipmapped: false)
        d.usage = [.renderTarget, .shaderRead]
        d.storageMode = .shared
        guard let target = device.makeTexture(descriptor: d), let cb = queue.makeCommandBuffer() else { return nil }
        let keep = encode(pb, into: target, format: .bgra8Unorm, commandBuffer: cb, zoomed: false)
        cb.commit()
        cb.waitUntilCompleted()
        _ = keep
        var bytes = [UInt8](repeating: 0, count: w * h * 4)
        target.getBytes(&bytes, bytesPerRow: w * 4, from: MTLRegionMake2D(0, 0, w, h), mipmapLevel: 0)
        guard let provider = CGDataProvider(data: Data(bytes) as CFData) else { return nil }
        return CGImage(
            width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: w * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }
}
