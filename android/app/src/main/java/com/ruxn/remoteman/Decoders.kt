package com.ruxn.remoteman

import android.media.MediaCodecInfo.CodecCapabilities
import android.media.MediaCodecInfo.CodecProfileLevel
import android.media.MediaCodecList
import android.os.Build

/** What this device decodes, for the host to choose from: "av1", "hevc" (always 10-bit 4:2:0 so far). */
object Decoders {
    val isEmulator: Boolean
        get() = Build.HARDWARE == "ranchu" || Build.FINGERPRINT.contains("generic")

    /**
     * The decoder to use for [mime] in [profile]: never one for protected content only, and one
     * with a low-latency mode if there is one. Qualcomm lists those apart
     * (`c2.qti.av1.decoder.low_latency` beside `c2.qti.av1.decoder`); others have one decoder
     * that does both (Google's on the Pixel).
     */
    fun has(mime: String, profile: Int, hardwareOnly: Boolean): String? {
        val fit = MediaCodecList(MediaCodecList.ALL_CODECS).codecInfos.filter { info ->
            if (info.isEncoder || mime !in info.supportedTypes) return@filter false
            if (hardwareOnly && !info.isHardwareAccelerated) return@filter false
            val caps = info.getCapabilitiesForType(mime)
            !caps.isFeatureRequired(CodecCapabilities.FEATURE_SecurePlayback) && caps.profileLevels.any { it.profile == profile }
        }
        val quick = fit.firstOrNull { it.getCapabilitiesForType(mime).isFeatureSupported(CodecCapabilities.FEATURE_LowLatency) }
        return (quick ?: fit.firstOrNull())?.name
    }

    /** The 10-bit decoder for each codec, told to the core, which otherwise takes the first of a type. */
    fun choose() {
        val hw = !isEmulator
        has("video/av01", CodecProfileLevel.AV1ProfileMain10, hw)?.let { Core.useDecoder("video/av01", it) }
        has("video/hevc", CodecProfileLevel.HEVCProfileMain10, hw)?.let { Core.useDecoder("video/hevc", it) }
    }

    /** In order of preference. The emulator has only software decoders, which do for testing. */
    fun list(): String {
        val hw = !isEmulator
        val out = mutableListOf<String>()
        if (has("video/av01", CodecProfileLevel.AV1ProfileMain10, hw) != null) out += "av1"
        if (has("video/hevc", CodecProfileLevel.HEVCProfileMain10, hw) != null) out += "hevc"
        return out.joinToString(",")
    }

    fun describe(): String {
        val hw = !isEmulator
        return listOf(
            "AV1 10-bit: " + (has("video/av01", CodecProfileLevel.AV1ProfileMain10, hw) ?: "none"),
            "HEVC 10-bit: " + (has("video/hevc", CodecProfileLevel.HEVCProfileMain10, hw) ?: "none"),
        ).joinToString("\n")
    }
}
