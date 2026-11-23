package com.xnixy.remoteman

import android.media.MediaCodecInfo.CodecProfileLevel
import android.media.MediaCodecList
import android.os.Build

/** What this device decodes, for the host to choose from: "av1", "hevc" (always 10-bit 4:2:0 so far). */
object Decoders {
    val isEmulator: Boolean
        get() = Build.HARDWARE == "ranchu" || Build.FINGERPRINT.contains("generic")

    private fun has(mime: String, profile: Int, hardwareOnly: Boolean): String? {
        for (info in MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos) {
            if (info.isEncoder || mime !in info.supportedTypes) continue
            if (hardwareOnly && !info.isHardwareAccelerated) continue
            val caps = info.getCapabilitiesForType(mime)
            if (caps.profileLevels.any { it.profile == profile }) return info.name
        }
        return null
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
