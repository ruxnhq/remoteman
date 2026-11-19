// A viewer written against the C interface alone, for testing it: connects, counts the frames
// of the first picture, asks for a second display and counts its frames too, says which
// display the pointer is on, asks for one the test host refuses, and gives the display back.
// Run by scripts/test-c-host.sh against fakehost.c.
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "remoteman_core.h"

static volatile int frames, frames2, keys2, streams2, gone, closed;
static volatile unsigned w2, h2;

static void on_stream(void *ctx, uint16_t epoch, uint8_t codec, uint16_t chroma, uint8_t depth, uint32_t w, uint32_t h, uint32_t refresh, uint8_t hdr) {
    printf("client: stream %ux%u at %u Hz\n", w, h, refresh);
}
static void on_frame(void *ctx, const uint8_t *data, size_t len, uint8_t key, uint32_t number, uint64_t capture_us, int64_t age_us) { frames++; }
static void on_closed(void *ctx, const char *reason) { printf("client: closed: %s\n", reason); closed = 1; }
static void on_message(void *ctx, const char *json) {
    if (strstr(json, "\"t\":\"display\"")) { printf("client: message %s\n", json); gone++; }
}
static void on_stream2(void *ctx, uint16_t epoch, uint8_t codec, uint16_t chroma, uint8_t depth, uint32_t w, uint32_t h, uint32_t refresh, uint8_t hdr) {
    printf("client: second display, epoch %u, codec %u, %ux%u at %u Hz\n", epoch, codec, w, h, refresh);
    streams2++; w2 = w; h2 = h;
}
static void on_frame2(void *ctx, const uint8_t *data, size_t len, uint8_t key, uint32_t number, uint64_t capture_us, int64_t age_us) {
    if (frames2 == 0 && !(key & 1)) printf("client: the second display's first frame is not a keyframe\n");
    frames2++; keys2 += key & 1;
}

int main(int argc, char **argv) {
    char err[256] = "";
    rm_callbacks cb = { 0 };
    cb.on_stream = on_stream; cb.on_frame = on_frame; cb.on_closed = on_closed; cb.on_message = on_message;
    cb.on_stream2 = on_stream2; cb.on_frame2 = on_frame2;
    setvbuf(stdout, NULL, _IOLBF, 0);
    rm_client *c = rm_client_connect(argv[1], argv[2], "C test client", 1, 1920, 1080, 1.0f, 120, "av1,hevc", NULL, cb, err, sizeof err);
    if (!c) { printf("client: failed: %s\n", err); return 1; }
    sleep(1);
    int before = frames2;
    rm_client_second_display(c, 1, 1280, 720, 1.0f, 60);
    sleep(2);
    rm_client_pointer_on(c, 1);
    rm_client_pointer_to(c, 0.5f, 0.5f);
    int asked = rm_client_send_message(c, "{\"t\":\"display_keyframe\",\"index\":1}");
    sleep(1);
    int first = frames2, first_keys = keys2;
    // Another size while it is on: the picture starts afresh.
    rm_client_second_display(c, 1, 1920, 1080, 1.0f, 60);
    sleep(1);
    int resized = streams2 == 2 && w2 == 1920 && h2 == 1080;
    rm_client_pointer_on(c, 0);
    rm_client_second_display(c, 0, 0, 0, 0, 0);
    sleep(1);
    int after = frames2;
    sleep(1);
    int still = frames2 == after;
    // One the test host refuses: the answer comes as a message.
    int told = gone;
    rm_client_second_display(c, 1, 1280, 5000, 1.0f, 60);
    sleep(1);
    int refused = gone == told + 1 && frames2 == after;
    printf("client: first picture %d frames; second before asking %d, then %d frames (%d keyframes, asked %d), resized %d, none after giving it back %d, refusal told %d\n",
           frames, before, first, first_keys, asked, resized, still, refused);
    int ok = frames > 300 && before == 0 && first > 120 && first_keys >= 2 && asked == 1 && resized && still && refused && !closed;
    rm_client_close(c);
    printf("client: %s\n", ok ? "PASS" : "FAIL");
    return !ok;
}
