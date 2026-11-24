#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <ImageIO/ImageIO.h>
#import <IOSurface/IOSurface.h>
#import <QuartzCore/QuartzCore.h>
#import <dispatch/dispatch.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <dlfcn.h>
#include "ghostty.h"

#define GHOSTTY_FUNCTIONS(X) \
    X(ghostty_init) X(ghostty_config_new) X(ghostty_config_finalize) \
    X(ghostty_app_new) X(ghostty_app_tick) \
    X(ghostty_app_free) X(ghostty_surface_config_new) X(ghostty_surface_new) \
    X(ghostty_surface_set_size) X(ghostty_surface_set_focus) \
    X(ghostty_surface_size) X(ghostty_surface_free)
#define DECLARE_GHOSTTY(name) static __typeof__(&name) tap_##name;
GHOSTTY_FUNCTIONS(DECLARE_GHOSTTY)
#undef DECLARE_GHOSTTY
#define ghostty_init tap_ghostty_init
#define ghostty_config_new tap_ghostty_config_new
#define ghostty_config_finalize tap_ghostty_config_finalize
#define ghostty_app_new tap_ghostty_app_new
#define ghostty_app_tick tap_ghostty_app_tick
#define ghostty_app_free tap_ghostty_app_free
#define ghostty_surface_config_new tap_ghostty_surface_config_new
#define ghostty_surface_new tap_ghostty_surface_new
#define ghostty_surface_set_size tap_ghostty_surface_set_size
#define ghostty_surface_set_focus tap_ghostty_surface_set_focus
#define ghostty_surface_size tap_ghostty_surface_size
#define ghostty_surface_free tap_ghostty_surface_free

static bool load_ghostty(void) {
    const char *path = getenv("TAP_GHOSTTY_STOCK");
    if (!path) path = "/Applications/Ghostty.app/Contents/MacOS/ghostty";
    const char *frameworks = getenv("TAP_GHOSTTY_FRAMEWORKS");
    if (!frameworks) frameworks = "/Applications/Ghostty.app/Contents/Frameworks";
    setenv("DYLD_FRAMEWORK_PATH", frameworks, 0);
    char sparkle[4096];
    snprintf(sparkle, sizeof(sparkle), "%s/Sparkle.framework/Versions/B/Sparkle", frameworks);
    dlopen(sparkle, RTLD_NOW | RTLD_GLOBAL);
    void *handle = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!handle) { fprintf(stderr, "dlopen stock Ghostty: %s\n", dlerror()); return false; }
#define LOAD_GHOSTTY(name) tap_##name = (__typeof__(tap_##name))dlsym(handle, #name); if (!tap_##name) { fprintf(stderr, "missing stock Ghostty symbol: %s\n", #name); return false; }
    GHOSTTY_FUNCTIONS(LOAD_GHOSTTY)
#undef LOAD_GHOSTTY
    return true;
}

static ghostty_app_t g_app = NULL;

static void wakeup_cb(void *userdata) {
    (void)userdata;
    dispatch_async(dispatch_get_main_queue(), ^{
        if (g_app) ghostty_app_tick(g_app);
    });
}

static bool action_cb(ghostty_app_t app, ghostty_target_s target, ghostty_action_s action) {
    (void)app;
    (void)target;
    if (action.tag == GHOSTTY_ACTION_RENDERER_HEALTH &&
        action.action.renderer_health != GHOSTTY_RENDERER_HEALTH_HEALTHY) {
        fprintf(stderr, "renderer reported unhealthy\n");
    }
    return true;
}

static ghostty_clipboard_read_result_e read_clipboard_cb(
    void *userdata,
    ghostty_clipboard_e clipboard,
    void *request,
    const char *const *mimes,
    size_t mimes_len,
    bool list) {
    (void)userdata; (void)clipboard; (void)request; (void)mimes; (void)mimes_len; (void)list;
    return GHOSTTY_CLIPBOARD_READ_UNAVAILABLE;
}

static void confirm_read_clipboard_cb(
    void *userdata,
    const ghostty_clipboard_confirm_s *confirm,
    void *request,
    ghostty_clipboard_request_e type) {
    (void)userdata; (void)confirm; (void)request; (void)type;
}

static void write_clipboard_cb(
    void *userdata,
    ghostty_clipboard_e clipboard,
    const ghostty_clipboard_content_s *contents,
    size_t contents_len,
    bool confirm) {
    (void)userdata; (void)clipboard; (void)contents; (void)contents_len; (void)confirm;
}

static void close_surface_cb(void *userdata, bool process_alive) {
    (void)userdata; (void)process_alive;
}

static void run_loop_for(NSTimeInterval seconds) {
    NSDate *until = [NSDate dateWithTimeIntervalSinceNow:seconds];
    while ([until timeIntervalSinceNow] > 0) {
        @autoreleasepool {
            [[NSRunLoop mainRunLoop] runMode:NSDefaultRunLoopMode
                                  beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
            if (g_app) ghostty_app_tick(g_app);
        }
    }
}

static bool capture_iosurface_png(CALayer *layer, NSString *path) {
    id contents = layer.contents;
    if (!contents) {
        fprintf(stderr, "layer.contents is nil\n");
        return false;
    }

    IOSurfaceRef surface = (__bridge IOSurfaceRef)contents;
    const size_t width = IOSurfaceGetWidth(surface);
    const size_t height = IOSurfaceGetHeight(surface);
    const size_t src_bpr = IOSurfaceGetBytesPerRow(surface);
    const OSType format = IOSurfaceGetPixelFormat(surface);
    fprintf(stderr, "IOSurface %zux%zu bpr=%zu format=0x%08x\n",
            width, height, src_bpr, (unsigned)format);

    if (IOSurfaceLock(surface, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess) {
        fprintf(stderr, "IOSurfaceLock failed\n");
        return false;
    }

    const uint8_t *src = IOSurfaceGetBaseAddress(surface);
    const size_t dst_bpr = width * 4;
    NSMutableData *rgba = [NSMutableData dataWithLength:dst_bpr * height];
    uint8_t *dst = rgba.mutableBytes;
    uint64_t nonzero_alpha = 0;
    uint64_t colored_pixels = 0;

    for (size_t y = 0; y < height; y++) {
        const uint8_t *s = src + y * src_bpr;
        uint8_t *d = dst + y * dst_bpr;
        for (size_t x = 0; x < width; x++) {
            // Ghostty's render target is 32BGRA. Convert to straight channel-order RGBA.
            d[4*x + 0] = s[4*x + 2];
            d[4*x + 1] = s[4*x + 1];
            d[4*x + 2] = s[4*x + 0];
            d[4*x + 3] = s[4*x + 3];
            if (s[4*x + 3] != 0) nonzero_alpha++;
            if (s[4*x + 0] != s[0] || s[4*x + 1] != s[1] || s[4*x + 2] != s[2]) colored_pixels++;
        }
    }
    IOSurfaceUnlock(surface, kIOSurfaceLockReadOnly, NULL);

    CGDataProviderRef provider = CGDataProviderCreateWithCFData((__bridge CFDataRef)rgba);
    // Ghostty tags its IOSurface render target as Display P3 (Target.zig).
    CGColorSpaceRef colorspace = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
    CGBitmapInfo info = kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big;
    CGImageRef image = CGImageCreate(width, height, 8, 32, dst_bpr, colorspace,
                                     info, provider, NULL, false,
                                     kCGRenderingIntentDefault);
    NSURL *url = [NSURL fileURLWithPath:path];
    CGImageDestinationRef dest = CGImageDestinationCreateWithURL(
        (__bridge CFURLRef)url, CFSTR("public.png"), 1, NULL);
    bool ok = false;
    if (dest && image) {
        CGImageDestinationAddImage(dest, image, NULL);
        ok = CGImageDestinationFinalize(dest);
    }
    fprintf(stderr, "capture nonzero_alpha=%llu colored_pixels=%llu png=%s\n",
            nonzero_alpha, colored_pixels, ok ? "yes" : "no");

    if (dest) CFRelease(dest);
    if (image) CGImageRelease(image);
    CGColorSpaceRelease(colorspace);
    CGDataProviderRelease(provider);
    return ok;
}

static bool file_exists(const char *path) { return access(path, F_OK) == 0; }

// Single-quote a path for /bin/sh.
static NSString *shell_quote(NSString *value) {
    return [NSString stringWithFormat:@"'%@'", [value stringByReplacingOccurrencesOfString:@"'" withString:@"'\\''"]];
}

// Grows the surface's pixel size from the cell size until Ghostty reports exactly cols x rows.
static bool size_surface(ghostty_surface_t surface, NSView *view, long cols, long rows) {
    ghostty_surface_size_s size = ghostty_surface_size(surface);
    const uint32_t cell_w = size.cell_width_px, cell_h = size.cell_height_px;
    if (!cell_w || !cell_h) { fprintf(stderr, "Ghostty reported no cell size\n"); return false; }
    uint32_t width = (uint32_t)cols * cell_w, height = (uint32_t)rows * cell_h;
    for (int attempt = 0; attempt < 256; attempt++) {
        ghostty_surface_set_size(surface, width, height);
        size = ghostty_surface_size(surface);
        if (size.columns >= cols && size.rows >= rows) break;
        if (size.columns < cols) width += 1;
        if (size.rows < rows) height += 1;
    }
    [view setFrameSize:NSMakeSize(width, height)];
    size = ghostty_surface_size(surface);
    fprintf(stderr, "terminal cols=%u rows=%u pixels=%ux%u cell=%ux%u\n",
            size.columns, size.rows, size.width_px, size.height_px, cell_w, cell_h);
    if (size.columns != cols || size.rows != rows) {
        fprintf(stderr, "could not size Ghostty to %ldx%ld\n", cols, rows);
        return false;
    }
    return true;
}

static bool wait_for_file(NSString *path, NSTimeInterval seconds) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:seconds];
    while (!file_exists(path.fileSystemRepresentation)) {
        if ([deadline timeIntervalSinceNow] < 0) return false;
        run_loop_for(0.01);
    }
    return true;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc != 3 && argc != 4) {
            fprintf(stderr, "usage: tap-ghostty-offscreen OUTPUT.png MANIFEST [FONT_SIZE]\n"
                            "  MANIFEST lines: <cols> <rows> <bytes-file>, replayed in order; the\n"
                            "  terminal is resized to each segment's grid before its bytes are written.\n");
            return 2;
        }
        NSString *manifest = [NSString stringWithUTF8String:argv[2]];
        NSString *contents = [NSString stringWithContentsOfFile:manifest encoding:NSUTF8StringEncoding error:NULL];
        NSMutableArray<NSArray<NSNumber *> *> *grids = [NSMutableArray array];
        for (NSString *line in [contents componentsSeparatedByString:@"\n"]) {
            NSArray<NSString *> *fields = [line componentsSeparatedByString:@" "];
            if (fields.count < 3) continue;
            const long cols = fields[0].integerValue, rows = fields[1].integerValue;
            if (cols < 2 || rows < 1 || cols > 1000 || rows > 1000) {
                fprintf(stderr, "invalid grid %ldx%ld in manifest\n", cols, rows);
                return 2;
            }
            [grids addObject:@[@(cols), @(rows)]];
        }
        if (grids.count == 0) { fprintf(stderr, "manifest has no segments\n"); return 2; }
        NSString *(^marker)(NSString *, NSUInteger) = ^NSString *(NSString *kind, NSUInteger index) {
            return [NSString stringWithFormat:@"%@.%@.%lu", manifest, kind, (unsigned long)index];
        };
        for (NSUInteger index = 0; index < grids.count; index++) {
            unlink(marker(@"ready", index).fileSystemRepresentation);
            unlink(marker(@"done", index).fileSystemRepresentation);
        }
        if (!load_ghostty()) return 1;
        [NSApplication sharedApplication];

        if (ghostty_init((uintptr_t)argc, argv) != GHOSTTY_SUCCESS) {
            fprintf(stderr, "ghostty_init failed\n");
            return 1;
        }

        ghostty_config_t config = ghostty_config_new();
        if (!config) {
            fprintf(stderr, "ghostty_config_new failed\n");
            return 1;
        }
        ghostty_config_finalize(config);

        ghostty_runtime_config_s runtime = {0};
        runtime.supports_selection_clipboard = false;
        runtime.wakeup_cb = wakeup_cb;
        runtime.action_cb = action_cb;
        runtime.read_clipboard_cb = read_clipboard_cb;
        runtime.confirm_read_clipboard_cb = confirm_read_clipboard_cb;
        runtime.write_clipboard_cb = write_clipboard_cb;
        runtime.close_surface_cb = close_surface_cb;

        g_app = ghostty_app_new(&runtime, config);
        if (!g_app) {
            fprintf(stderr, "ghostty_app_new failed\n");
            return 1;
        }

        // Ghostty always starts commands through login(1) on macOS, which may print a banner, and it
        // answers any terminal queries in the replayed bytes by writing to the PTY. The replay script
        // therefore switches the tty to raw/no-echo (answers are never echoed). For each segment it
        // waits until the helper has resized the surface AND the tty reports that size (Ghostty
        // applies resizes on its IO thread, after the request), resets the terminal before the
        // first segment (clearing anything login printed), writes the segment's recorded bytes
        // verbatim, and signals completion. So output lands at the size it was recorded at, and
        // each resize reflows the screen just as it did while recording.
        NSString *script = [NSString stringWithFormat:
            @"/usr/bin/env /bin/sh -c 'stty raw -echo < /dev/tty; i=0; "
             "while read -r cols rows file; do "
             "while [ ! -e \"$0.ready.$i\" ]; do sleep 0.01; done; "
             "until [ \"$(stty size < /dev/tty)\" = \"$rows $cols\" ]; do sleep 0.01; done; "
             "[ $i -eq 0 ] && printf \"\\033c\"; "
             "cat \"$file\"; : > \"$0.done.$i\"; i=$((i+1)); "
             "done < \"$0\"; exec sleep 600' %@",
            shell_quote(manifest)];

        NSView *view = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, 800, 600)];

        ghostty_surface_config_s surface_config = ghostty_surface_config_new();
        surface_config.platform_tag = GHOSTTY_PLATFORM_MACOS;
        surface_config.platform.macos.nsview = (__bridge void *)view;
        surface_config.scale_factor = 1.0;
        surface_config.font_size = argc == 4 ? (float)strtod(argv[3], NULL) : 16.0f;
        surface_config.command = script.UTF8String;
        surface_config.wait_after_command = true;

        ghostty_surface_t surface = ghostty_surface_new(g_app, &surface_config);
        if (!surface) {
            fprintf(stderr, "ghostty_surface_new failed\n");
            ghostty_app_free(g_app);
            return 1;
        }
        ghostty_surface_set_size(surface, 800, 600);
        ghostty_surface_set_focus(surface, true);
        run_loop_for(0.05);

        for (NSUInteger index = 0; index < grids.count; index++) {
            if (!size_surface(surface, view, grids[index][0].longValue, grids[index][1].longValue)) return 1;
            FILE *file = fopen(marker(@"ready", index).fileSystemRepresentation, "w");
            if (file) fclose(file);
            if (!wait_for_file(marker(@"done", index), 60)) {
                fprintf(stderr, "replay timed out in segment %lu\n", (unsigned long)index);
                return 1;
            }
        }
        // Let Ghostty's IO thread finish parsing and the renderer draw the final state.
        run_loop_for(0.15);

        CALayer *layer = view.layer;
        fprintf(stderr, "view wantsLayer=%d layer=%s contents=%s\n",
                view.wantsLayer,
                NSStringFromClass(layer.class).UTF8String,
                layer.contents ? NSStringFromClass([layer.contents class]).UTF8String : "nil");

        bool ok = capture_iosurface_png(layer, [NSString stringWithUTF8String:argv[1]]);

        ghostty_surface_free(surface);
        ghostty_app_free(g_app);
        g_app = NULL;
        for (NSUInteger index = 0; index < grids.count; index++) {
            unlink(marker(@"ready", index).fileSystemRepresentation);
            unlink(marker(@"done", index).fileSystemRepresentation);
        }
        return ok ? 0 : 1;
    }
}
