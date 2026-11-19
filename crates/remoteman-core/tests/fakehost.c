// A host written against the C interface alone, for testing it: accepts any client, sends 120
// made-up frames a second (the first a "keyframe"), reports a cursor, and prints the input it
// receives. Asked for a second display, it sends 60 made-up frames a second of that too. Run by
// scripts/test-c-host.sh against the real test client and against fakeclient.c.
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "remoteman_core.h"

static volatile int connected, inputs, keyframes;
// The second display: 1 asked for (start it), 2 running, 3 a keyframe asked for, 0 none.
static volatile int second, second_keyframes, on_second;
static volatile unsigned second_w, second_h;

static int32_t on_client(void *ctx, const rm_client_info *c, rm_accept *a) {
    printf("host: client \"%s\" %ux%u scale %.2f at %u Hz, decoders %s, cursor apart %d, placement %d\n", c->name, c->width, c->height, c->scale, c->refresh, c->decoders, c->cursor_apart, c->placement);
    if (c->width > 4000) { snprintf(a->refuse, sizeof a->refuse, "too wide for the test host"); return 0; }
    a->codec = 1; a->depth = 10; a->chroma = 420; a->width = c->width; a->height = c->height; a->refresh = c->refresh; a->hdr = 0;
    connected = 1;
    return 1;
}
static void on_keyframe(void *ctx) { keyframes++; }
static void on_input(void *ctx, const rm_input *i) {
    if (i->kind == 8) { on_second = i->slot; printf("host: the pointer is on display %d\n", i->slot); return; }
    if (inputs++ < 3 || i->kind == 7) printf("host: input kind %d x %.2f y %.2f usage %d text %s\n", i->kind, i->x, i->y, i->usage, i->text ? i->text : "-");
}
static void on_message(void *ctx, const char *json) {
    if (strstr(json, "\"t\":\"display_keyframe\"")) { second_keyframes++; if (second == 2) second = 3; return; }
    if (!strstr(json, "\"t\":\"display\"")) return;
    printf("host: message %s\n", json);
    if (strstr(json, "\"on\":true")) {
        const char *w = strstr(json, "\"width\":"), *h = strstr(json, "\"height\":");
        if (w && h) { sscanf(w + 8, "%u", (unsigned *)&second_w); sscanf(h + 9, "%u", (unsigned *)&second_h); }
        second = 1;
    } else second = 0;
}
static void on_left(void *ctx) { second = 0; printf("host: client left after %d inputs, %d keyframe requests\n", inputs, keyframes); connected = 0; fflush(stdout); }

int main(int argc, char **argv) {
    char err[256] = "", info[200];
    static uint8_t frame[30000];
    static const uint8_t dot[4 * 4 * 4] = { 255 };
    rm_host_callbacks cb = { NULL, on_client, on_keyframe, on_input, on_left, on_message };
    setvbuf(stdout, NULL, _IOLBF, 0);
    rm_host *h = rm_host_start(argv[1], argv[2], "C test host", "test", "video,input,cursor,displays", 1, cb, err, sizeof err);
    if (!h) { printf("host: failed: %s\n", err); return 1; }
    rm_host_info(h, info, sizeof info);
    printf("host: %s pairing %d\n", info, rm_host_pairing(h));
    fflush(stdout);
    memset(frame, 0x5a, sizeof frame);
    for (int tick = 0, n = 0; tick < 120 * 25; tick++) {
        usleep(8333);
        if (!connected) { n = 0; continue; }
        if (n == 0) rm_host_cursor_shape(h, dot, 4, 4, 1, 1);
        rm_host_cursor_at(h, (n % 100) / 100.0f, 0.5f, 1);
        rm_host_send_frame(h, frame, n == 0 ? sizeof frame : 4000, n == 0, rm_host_now_us(h));
        n++;
        // The second display, at half the rate. One too tall is refused, to try that.
        if (second == 1 && second_h > 4000) { rm_host_second_stop(h, "too tall for the test host"); second = 0; }
        if (second == 1 || second == 3) {
            int afresh = second == 1;
            rm_accept a = { 0 };
            a.codec = 0; a.depth = 8; a.chroma = 420; a.width = second_w; a.height = second_h; a.refresh = 60;
            if (afresh) rm_host_second_start(h, &a);
            second = 2;
            rm_host_send_frame2(h, frame, 20000, 1, rm_host_now_us(h));
        } else if (second == 2 && tick % 2 == 0) rm_host_send_frame2(h, frame, 3000, 0, rm_host_now_us(h));
    }
    rm_host_stop(h);
    return 0;
}
