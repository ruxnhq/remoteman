# Where the AppImages' programs are built: Ubuntu 25.10, the oldest release with the GTK the
# viewer needs (4.20), so what is built here runs there and on anything newer (glibc 2.42).
# FFmpeg is built here too, with only what remoteman uses and nothing under the GPL, so it can
# go inside the AppImages. Used by packaging/appimage/build.sh, from the repository's root.
FROM ubuntu:25.10
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config nasm clang libclang-dev ca-certificates curl xz-utils file patchelf squashfs-tools desktop-file-utils \
    libgtk-4-dev libadwaita-1-dev libepoxy-dev libwayland-dev libpipewire-0.3-dev libopus-dev libpam0g-dev \
    libva-dev libdrm-dev libvulkan-dev libdav1d-dev libopenh264-dev \
    && rm -rf /var/lib/apt/lists/*
# NVIDIA's codec headers as FFmpeg looks for them (the repository carries them; nothing is
# linked: the driver's libraries are loaded when the program runs).
COPY vendor/nv-codec-headers /usr/local/include/ffnvcodec
RUN printf 'prefix=/usr/local\nincludedir=${prefix}/include\n\nName: ffnvcodec\nDescription: FFmpeg version of Nvidia Codec SDK headers\nVersion: 13.0.19.0\nCflags: -I${includedir}\n' > /usr/lib/x86_64-linux-gnu/pkgconfig/ffnvcodec.pc
# Its licence's text and which build it is (the release, its source's sha256, the configure
# line) are kept in /opt/ffmpeg/share/licenses/ffmpeg before the source is deleted:
# assemble.sh puts them in the AppImages.
ARG FFMPEG=9.0.2
RUN curl -fsSL "https://ffmpeg.org/releases/ffmpeg-${FFMPEG}.tar.xz" -o /tmp/ffmpeg.tar.xz && mkdir /tmp/ffmpeg && tar -xf /tmp/ffmpeg.tar.xz -C /tmp/ffmpeg --strip-components=1 \
    && cd /tmp/ffmpeg && ./configure --prefix=/opt/ffmpeg --disable-static --enable-shared --disable-programs --disable-doc --disable-debug \
       --disable-everything --disable-network --disable-avdevice --disable-avfilter --disable-xlib --disable-vdpau \
       --enable-decoder=hevc,av1,libdav1d,h264,mjpeg --enable-parser=hevc,av1,h264,mjpeg \
       --enable-encoder=h264_nvenc,h264_vaapi,hevc_vaapi,av1_vaapi,libopenh264 \
       --enable-hwaccel=hevc_nvdec,av1_nvdec,hevc_vaapi,av1_vaapi,hevc_vulkan,av1_vulkan \
       --enable-libdav1d --enable-libopenh264 --enable-nvenc --enable-nvdec --enable-vaapi --enable-vulkan \
    && make -j"$(nproc)" && make install \
    && mkdir -p /opt/ffmpeg/share/licenses/ffmpeg && cp COPYING.LGPLv2.1 LICENSE.md /opt/ffmpeg/share/licenses/ffmpeg/ \
    && printf 'FFmpeg %s, from https://ffmpeg.org/releases/ffmpeg-%s.tar.xz\n  (sha256 %s), not changed\nconfiguration: %s\n' \
       "$FFMPEG" "$FFMPEG" "$(sha256sum /tmp/ffmpeg.tar.xz | cut -d' ' -f1)" "$(sed -n 's/^#define FFMPEG_CONFIGURATION "\(.*\)"$/\1/p' config.h)" \
       > /opt/ffmpeg/share/licenses/ffmpeg/build.txt \
    && cd / && rm -rf /tmp/ffmpeg /tmp/ffmpeg.tar.xz
# The C wrapper over VA-API (crates/remoteman-encode) includes FFmpeg's headers and links its
# libraries by their names, without asking pkg-config where they are.
ENV PKG_CONFIG_PATH=/opt/ffmpeg/lib/pkgconfig LD_LIBRARY_PATH=/opt/ffmpeg/lib C_INCLUDE_PATH=/opt/ffmpeg/include LIBRARY_PATH=/opt/ffmpeg/lib
# What an AppImage starts with: the AppImage project's own runtime, to which the squashed
# folder is appended (packaging/appimage/assemble.sh).
RUN curl -fsSL https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64 -o /opt/appimage-runtime && sha256sum /opt/appimage-runtime
# The shader compiler the host's GPU code is built with.
RUN apt-get update && apt-get install -y --no-install-recommends glslc && rm -rf /var/lib/apt/lists/*
