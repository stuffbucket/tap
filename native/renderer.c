#include <cairo.h>
#ifdef __APPLE__
#include <CoreText/CoreText.h>
#include <cairo-quartz.h>
#endif
#include <ghostty/vt.h>
#include <png.h>

#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define OP_WRITE 1
#define OP_RESIZE 2
#define OP_FRAME 3
#define OP_RESET 4
#define OP_QUIT 5
#define OP_SCROLL 6
#define OP_TICK 7

#define STYLE_BOLD (1u << 0)
#define STYLE_ITALIC (1u << 1)
#define STYLE_FAINT (1u << 2)
#define STYLE_INVERSE (1u << 4)
#define STYLE_INVISIBLE (1u << 5)
#define STYLE_STRIKETHROUGH (1u << 6)
#define STYLE_OVERLINE (1u << 7)

#define MAX_REQUEST_BYTES (64u * 1024u * 1024u)
#define MAX_IMAGE_PIXELS (64u * 1024u * 1024u)

static uint16_t g_cols;
static uint16_t g_rows;
static uint32_t g_cell_width;
static uint32_t g_cell_height;
static double g_font_size;
static const char *g_font_family = "JetBrains Mono";
static GhosttyColorRgb g_background = {0, 0, 0};
static GhosttyColorRgb g_foreground = {255, 255, 255};
static GhosttyColorRgb g_palette[256];
static bool g_palette_set;
static bool g_animation_clock_initialized;
static uint64_t g_animation_time_ms;
static GhosttyTerminal g_terminal;
static GhosttyRenderState g_render_state;
static GhosttyRenderStateRowIterator g_rows_iter;

#ifdef __APPLE__
// On macOS Cairo's toy font API looks fonts up through CoreText by exact name and falls back to
// Helvetica, a proportional face. Instead, register the bundled fonts for this process and
// resolve the requested family (regular, bold, italic, bold italic) through CoreText's own
// family matching, once.
typedef struct {
  cairo_font_face_t *face;
  CTFontRef font;  // the same face for CoreText coverage checks and fallback
  bool oblique;    // the family has no italic: slant the upright face instead
} TapFace;
static TapFace g_faces[4];  // regular, bold, italic, bold italic

// Faces CoreText picked for characters the primary family does not cover, by PostScript name.
typedef struct {
  CFStringRef name;
  cairo_font_face_t *face;
} FallbackFace;
static FallbackFace g_fallbacks[64];
static size_t g_fallback_count;

static void register_font_directory(const char *directory) {
  CFStringRef path = CFStringCreateWithCString(NULL, directory, kCFStringEncodingUTF8);
  if (!path) return;
  CFURLRef url = CFURLCreateWithFileSystemPath(NULL, path, kCFURLPOSIXPathStyle, true);
  CFRelease(path);
  if (!url) return;
  CFURLEnumeratorRef enumerator = CFURLEnumeratorCreateForDirectoryURL(NULL, url, kCFURLEnumeratorDefaultBehavior, NULL);
  CFURLRef file = NULL;
  while (enumerator && CFURLEnumeratorGetNextURL(enumerator, &file, NULL) == kCFURLEnumeratorSuccess) {
    CFStringRef extension = CFURLCopyPathExtension(file);
    if (extension && (CFStringCompare(extension, CFSTR("ttf"), kCFCompareCaseInsensitive) == kCFCompareEqualTo ||
                      CFStringCompare(extension, CFSTR("otf"), kCFCompareCaseInsensitive) == kCFCompareEqualTo)) {
      CTFontManagerRegisterFontsForURL(file, kCTFontManagerScopeProcess, NULL);
    }
    if (extension) CFRelease(extension);
  }
  if (enumerator) CFRelease(enumerator);
  CFRelease(url);
}

// Characters missing from the primary family are looked up in the bundled symbol fonts first
// (Braille spinners, Nerd Font icons, monochrome emoji), then in the system cascade (CJK, ...).
static CFArrayRef bundled_cascade(void) {
  const char *names[] = {"NotoSansSymbols2-Regular", "SymbolsNFM", "NotoEmoji-Regular"};
  CFMutableArrayRef list = CFArrayCreateMutable(NULL, 0, &kCFTypeArrayCallBacks);
  for (size_t index = 0; index < sizeof(names) / sizeof(names[0]); ++index) {
    CFStringRef name = CFStringCreateWithCString(NULL, names[index], kCFStringEncodingUTF8);
    CTFontDescriptorRef descriptor = CTFontDescriptorCreateWithNameAndSize(name, 0);
    CFArrayAppendValue(list, descriptor);
    CFRelease(descriptor);
    CFRelease(name);
  }
  return list;
}

// A CoreText font of `family` with the given traits, or NULL when the family is not available.
static CTFontRef family_font(const char *family, CTFontSymbolicTraits traits) {
  CFStringRef name = CFStringCreateWithCString(NULL, family, kCFStringEncodingUTF8);
  if (!name) return NULL;
  CFArrayRef cascade = bundled_cascade();
  const void *keys[] = {kCTFontFamilyNameAttribute, kCTFontCascadeListAttribute};
  const void *values[] = {name, cascade};
  CFDictionaryRef attributes = CFDictionaryCreate(NULL, keys, values, 2, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
  CFRelease(cascade);
  CTFontDescriptorRef descriptor = CTFontDescriptorCreateWithAttributes(attributes);
  CFRelease(attributes);
  CTFontRef base = CTFontCreateWithFontDescriptor(descriptor, g_font_size, NULL);
  CFRelease(descriptor);
  // CoreText substitutes another family rather than failing; treat that as "not available".
  CFStringRef resolved = base ? CTFontCopyFamilyName(base) : NULL;
  const bool matched = resolved && CFStringCompare(resolved, name, kCFCompareCaseInsensitive) == kCFCompareEqualTo;
  if (resolved) CFRelease(resolved);
  CFRelease(name);
  if (!matched) {
    if (base) CFRelease(base);
    return NULL;
  }
  if (!traits) return base;
  CTFontRef styled = CTFontCreateCopyWithSymbolicTraits(base, g_font_size, NULL, traits, kCTFontBoldTrait | kCTFontItalicTrait);
  if (!styled) return base;  // the family has no such style; use its regular face
  CFRelease(base);
  return styled;
}

static void load_faces(void) {
  const char *font_dir = getenv("TAP_FONT_DIR");
  if (font_dir && *font_dir) register_font_directory(font_dir);
  const char *family = g_font_family;
  CTFontRef probe = family_font(family, 0);
  if (!probe) {
    fprintf(stderr, "font family \"%s\" is not available; using JetBrains Mono\n", family);
    family = "JetBrains Mono";
    probe = family_font(family, 0);
    if (!probe) return;  // fall back to Cairo's toy font API
  }
  CFRelease(probe);
  const CTFontSymbolicTraits traits[4] = {0, kCTFontBoldTrait, kCTFontItalicTrait, kCTFontBoldTrait | kCTFontItalicTrait};
  for (int index = 0; index < 4; ++index) {
    CTFontRef font = family_font(family, traits[index]);
    if (!font) continue;
    CGFontRef graphics = CTFontCopyGraphicsFont(font, NULL);
    if (!graphics) {
      CFRelease(font);
      continue;
    }
    g_faces[index].face = cairo_quartz_font_face_create_for_cgfont(graphics);
    g_faces[index].font = font;
    g_faces[index].oblique = (traits[index] & kCTFontItalicTrait) && !(CTFontGetSymbolicTraits(font) & kCTFontItalicTrait);
    CGFontRelease(graphics);
  }
}

// A face for `utf8` when the primary font lacks any of its characters, or NULL to keep the primary.
static cairo_font_face_t *fallback_face(CTFontRef primary, const uint8_t *utf8, size_t length) {
  CFStringRef text = CFStringCreateWithBytes(NULL, utf8, (CFIndex)length, kCFStringEncodingUTF8, false);
  if (!text) return NULL;
  const CFIndex count = CFStringGetLength(text);
  UniChar characters[64];
  CGGlyph glyphs[64];
  if (count <= 0 || count > 64) {
    CFRelease(text);
    return NULL;
  }
  CFStringGetCharacters(text, CFRangeMake(0, count), characters);
  if (CTFontGetGlyphsForCharacters(primary, characters, glyphs, count)) {
    CFRelease(text);
    return NULL;
  }
  CTFontRef font = CTFontCreateForString(primary, text, CFRangeMake(0, count));
  CFRelease(text);
  if (!font) return NULL;
  CFStringRef name = CTFontCopyPostScriptName(font);
  cairo_font_face_t *face = NULL;
  for (size_t index = 0; index < g_fallback_count && !face; ++index) {
    if (CFEqual(g_fallbacks[index].name, name)) face = g_fallbacks[index].face;
  }
  if (!face && g_fallback_count < sizeof(g_fallbacks) / sizeof(g_fallbacks[0])) {
    CGFontRef graphics = CTFontCopyGraphicsFont(font, NULL);
    if (graphics) {
      face = cairo_quartz_font_face_create_for_cgfont(graphics);
      CGFontRelease(graphics);
      g_fallbacks[g_fallback_count++] = (FallbackFace){.name = (CFStringRef)CFRetain(name), .face = face};
    }
  }
  CFRelease(name);
  CFRelease(font);
  return face;
}

static void release_faces(void) {
  for (int index = 0; index < 4; ++index) {
    if (g_faces[index].face) cairo_font_face_destroy(g_faces[index].face);
    if (g_faces[index].font) CFRelease(g_faces[index].font);
    g_faces[index] = (TapFace){0};
  }
  for (size_t index = 0; index < g_fallback_count; ++index) {
    cairo_font_face_destroy(g_fallbacks[index].face);
    CFRelease(g_fallbacks[index].name);
  }
  g_fallback_count = 0;
}
#endif
static GhosttyRenderStateRowCells g_cells_iter;
static GhosttyRenderStateImageIterator g_images_iter;

typedef struct {
  uint8_t *data;
  size_t len;
  size_t cap;
} ByteBuffer;

static ByteBuffer g_replies;

typedef struct {
  GhosttyImageLayer layer;
  int32_t z;
  uint32_t image_id;
  uint64_t generation;
  int32_t col;
  int32_t row;
  uint32_t offset_x;
  uint32_t offset_y;
  uint32_t width;
  uint32_t height;
  uint32_t source_x;
  uint32_t source_y;
  uint32_t source_width;
  uint32_t source_height;
} ImagePlacement;

static bool read_exact(void *out, size_t len) {
  uint8_t *cursor = out;
  while (len > 0) {
    size_t count = fread(cursor, 1, len, stdin);
    if (count == 0) return false;
    cursor += count;
    len -= count;
  }
  return true;
}

static bool write_exact(const void *data, size_t len) {
  const uint8_t *cursor = data;
  while (len > 0) {
    size_t count = fwrite(cursor, 1, len, stdout);
    if (count == 0) return false;
    cursor += count;
    len -= count;
  }
  return true;
}

static uint32_t u32le(const uint8_t *p) {
  return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) |
         ((uint32_t)p[3] << 24);
}

static int32_t i32le(const uint8_t *p) {
  return (int32_t)u32le(p);
}

static uint64_t u64le(const uint8_t *p) {
  uint64_t value = 0;
  for (int i = 7; i >= 0; --i) value = (value << 8) | p[i];
  return value;
}

static void put_u32le(uint8_t *p, uint32_t value) {
  p[0] = (uint8_t)value;
  p[1] = (uint8_t)(value >> 8);
  p[2] = (uint8_t)(value >> 16);
  p[3] = (uint8_t)(value >> 24);
}

static bool respond(int32_t status, const uint8_t *payload, uint32_t len) {
  uint8_t header[8];
  put_u32le(header, (uint32_t)status);
  put_u32le(header + 4, len);
  if (!write_exact(header, sizeof(header))) return false;
  if (len > 0 && !write_exact(payload, len)) return false;
  return fflush(stdout) == 0;
}

static bool buffer_append(ByteBuffer *buffer, const uint8_t *data, size_t len) {
  if (len > SIZE_MAX - buffer->len) return false;
  const size_t needed = buffer->len + len;
  if (needed > buffer->cap) {
    size_t cap = buffer->cap == 0 ? 65536 : buffer->cap;
    while (cap < needed) {
      if (cap > SIZE_MAX / 2) return false;
      cap *= 2;
    }
    uint8_t *next = realloc(buffer->data, cap);
    if (!next) return false;
    buffer->data = next;
    buffer->cap = cap;
  }
  memcpy(buffer->data + buffer->len, data, len);
  buffer->len += len;
  return true;
}

static void write_pty(GhosttyTerminal terminal, void *userdata,
                      const uint8_t *data, size_t len) {
  (void)terminal;
  (void)userdata;
  buffer_append(&g_replies, data, len);
}

static cairo_status_t png_write(void *closure, const unsigned char *data,
                                unsigned int length) {
  return buffer_append(closure, data, length) ? CAIRO_STATUS_SUCCESS
                                               : CAIRO_STATUS_WRITE_ERROR;
}

static bool decode_png(void *userdata, const GhosttyAllocator *allocator,
                       const uint8_t *data, size_t data_len,
                       GhosttySysImage *out) {
  (void)userdata;
  png_image image;
  memset(&image, 0, sizeof(image));
  image.version = PNG_IMAGE_VERSION;
  if (!png_image_begin_read_from_memory(&image, data, data_len)) return false;
  image.format = PNG_FORMAT_RGBA;
  if (image.width == 0 || image.height == 0 ||
      (uint64_t)image.width * image.height > MAX_IMAGE_PIXELS) {
    png_image_free(&image);
    return false;
  }
  const size_t size = PNG_IMAGE_SIZE(image);
  uint8_t *pixels = ghostty_alloc(allocator, size);
  if (!pixels) {
    png_image_free(&image);
    return false;
  }
  if (!png_image_finish_read(&image, NULL, pixels, 0, NULL)) {
    ghostty_free(allocator, pixels, size);
    png_image_free(&image);
    return false;
  }
  out->width = image.width;
  out->height = image.height;
  out->data = pixels;
  out->data_len = size;
  png_image_free(&image);
  return true;
}

static void set_rgb(cairo_t *cr, GhosttyColorRgb color, double alpha) {
  cairo_set_source_rgba(cr, color.r / 255.0, color.g / 255.0,
                        color.b / 255.0, alpha);
}

static void fill(cairo_t *cr, double x, double y, double width, double height,
                 GhosttyColorRgb color) {
  set_rgb(cr, color, 1.0);
  cairo_rectangle(cr, x, y, width, height);
  cairo_fill(cr);
}

static bool get_colors(GhosttyRenderStateColors *colors) {
  memset(colors, 0, sizeof(*colors));
  colors->size = sizeof(*colors);
  return ghostty_render_state_get(g_render_state,
                                  GHOSTTY_RENDER_STATE_DATA_COLORS,
                                  colors) == GHOSTTY_SUCCESS;
}

static bool prepare_row_iterator(void) {
  return ghostty_render_state_get(g_render_state,
                                  GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                                  &g_rows_iter) == GHOSTTY_SUCCESS;
}

static bool prepare_cells(void) {
  return ghostty_render_state_row_get(g_rows_iter,
                                      GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                                      &g_cells_iter) == GHOSTTY_SUCCESS;
}

static void draw_backgrounds(cairo_t *cr, const GhosttyRenderStateColors *colors) {
  if (!prepare_row_iterator()) return;
  while (ghostty_render_state_row_iterator_next(g_rows_iter)) {
    int32_t y = 0;
    ghostty_render_state_row_get(g_rows_iter,
                                 GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y, &y);
    if (y < 0 || y >= g_rows || !prepare_cells()) continue;
    uint16_t x = 0;
    while (ghostty_render_state_row_cells_next(g_cells_iter) && x < g_cols) {
      GhosttyColorRgb background = colors->background;
      GhosttyResult result = ghostty_render_state_row_cells_get(
          g_cells_iter, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
          &background);
      if (result == GHOSTTY_SUCCESS) {
        fill(cr, x * g_cell_width, y * g_cell_height, g_cell_width,
             g_cell_height, background);
      }
      ++x;
    }
  }
}

#ifdef __APPLE__
static int face_index(GhosttyStyle style) {
  return ((style.flags & STYLE_BOLD) ? 1 : 0) | ((style.flags & STYLE_ITALIC) ? 2 : 0);
}
#endif

static void select_font(cairo_t *cr, GhosttyStyle style) {
#ifdef __APPLE__
  const TapFace *face = &g_faces[face_index(style)];
  if (face->face) {
    cairo_set_font_face(cr, face->face);
    if (face->oblique) {
      cairo_matrix_t matrix;
      cairo_matrix_init(&matrix, g_font_size, 0, -0.2 * g_font_size, g_font_size, 0, 0);
      cairo_set_font_matrix(cr, &matrix);
    } else {
      cairo_set_font_size(cr, g_font_size);
    }
    return;
  }
#endif
  cairo_font_slant_t slant = (style.flags & STYLE_ITALIC)
                                 ? CAIRO_FONT_SLANT_ITALIC
                                 : CAIRO_FONT_SLANT_NORMAL;
  cairo_font_weight_t weight = (style.flags & STYLE_BOLD)
                                   ? CAIRO_FONT_WEIGHT_BOLD
                                   : CAIRO_FONT_WEIGHT_NORMAL;
  cairo_select_font_face(cr, g_font_family, slant, weight);
  cairo_set_font_size(cr, g_font_size);
}

static void draw_decorations(cairo_t *cr, GhosttyStyle style,
                             GhosttyColorRgb color, double x, double y) {
  set_rgb(cr, color, 1.0);
  cairo_set_line_width(cr, 1.0);
  const int underline = (style.flags >> 8) & 7;
  if (underline != 0) {
    double underline_y = y + g_cell_height - 2;
    cairo_move_to(cr, x, underline_y);
    cairo_line_to(cr, x + g_cell_width, underline_y);
    cairo_stroke(cr);
  }
  if (style.flags & STYLE_STRIKETHROUGH) {
    double strike_y = y + g_cell_height * 0.55;
    cairo_move_to(cr, x, strike_y);
    cairo_line_to(cr, x + g_cell_width, strike_y);
    cairo_stroke(cr);
  }
  if (style.flags & STYLE_OVERLINE) {
    cairo_move_to(cr, x, y + 1);
    cairo_line_to(cr, x + g_cell_width, y + 1);
    cairo_stroke(cr);
  }
}

static void draw_text(cairo_t *cr, const GhosttyRenderStateColors *colors) {
  if (!prepare_row_iterator()) return;
  while (ghostty_render_state_row_iterator_next(g_rows_iter)) {
    int32_t y = 0;
    ghostty_render_state_row_get(g_rows_iter,
                                 GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y, &y);
    if (y < 0 || y >= g_rows || !prepare_cells()) continue;
    uint16_t x = 0;
    while (ghostty_render_state_row_cells_next(g_cells_iter) && x < g_cols) {
      GhosttyStyle style;
      memset(&style, 0, sizeof(style));
      ghostty_render_state_row_cells_get(
          g_cells_iter, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &style);

      uint8_t utf8[256];
      GhosttyBuffer grapheme = {.ptr = utf8, .cap = sizeof(utf8), .len = 0};
      GhosttyResult text_result = ghostty_render_state_row_cells_get(
          g_cells_iter, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
          &grapheme);
      if (text_result == GHOSTTY_SUCCESS && grapheme.len > 0 &&
          !(style.flags & STYLE_INVISIBLE)) {
        const size_t len = grapheme.len < sizeof(utf8) - 1
                               ? grapheme.len
                               : sizeof(utf8) - 1;
        utf8[len] = 0;
        GhosttyColorRgb foreground = colors->foreground;
        GhosttyColorRgb background = colors->background;
        ghostty_render_state_row_cells_get(
            g_cells_iter, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR,
            &foreground);
        ghostty_render_state_row_cells_get(
            g_cells_iter, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
            &background);
        if (style.flags & STYLE_INVERSE) {
          GhosttyColorRgb tmp = foreground;
          foreground = background;
          background = tmp;
          fill(cr, x * g_cell_width, y * g_cell_height, g_cell_width,
               g_cell_height, background);
        }
        select_font(cr, style);
        // Every glyph sits on the primary font's baseline, centred vertically in the cell by
        // ascent + descent, so punctuation, underscores and descenders keep their positions.
        cairo_font_extents_t font_extents;
        cairo_font_extents(cr, &font_extents);
#ifdef __APPLE__
        if (g_faces[0].font) {
          cairo_font_face_t *fallback = fallback_face(g_faces[0].font, utf8, len);
          if (fallback) cairo_set_font_face(cr, fallback);
        }
#endif
        set_rgb(cr, foreground, (style.flags & STYLE_FAINT) ? 0.55 : 1.0);
        cairo_text_extents_t extents;
        cairo_text_extents(cr, (const char *)utf8, &extents);
        double px = x * g_cell_width + (g_cell_width - extents.x_advance) / 2.0;
        double py = y * g_cell_height +
                    (g_cell_height - (font_extents.ascent + font_extents.descent)) / 2.0 +
                    font_extents.ascent;
        cairo_move_to(cr, px, py);
        cairo_show_text(cr, (const char *)utf8);
        draw_decorations(cr, style, foreground, x * g_cell_width,
                         y * g_cell_height);
      }
      ++x;
    }
  }
}

static int placement_compare(const void *left, const void *right) {
  const ImagePlacement *a = left;
  const ImagePlacement *b = right;
  if (a->layer != b->layer) return (int)a->layer - (int)b->layer;
  if (a->z < b->z) return -1;
  if (a->z > b->z) return 1;
  return 0;
}

static ImagePlacement *collect_images(size_t *out_count) {
  *out_count = 0;
  if (ghostty_render_state_get(g_render_state,
                               GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR,
                               &g_images_iter) != GHOSTTY_SUCCESS) {
    return NULL;
  }
  size_t count = 0;
  size_t cap = 0;
  ImagePlacement *placements = NULL;
  while (ghostty_render_state_image_iterator_next(g_images_iter)) {
    if (count == cap) {
      size_t next_cap = cap == 0 ? 8 : cap * 2;
      ImagePlacement *next = realloc(placements, next_cap * sizeof(*next));
      if (!next) {
        free(placements);
        return NULL;
      }
      placements = next;
      cap = next_cap;
    }
    ImagePlacement *p = &placements[count];
    memset(p, 0, sizeof(*p));
    GhosttyImageProtocol protocol = GHOSTTY_IMAGE_PROTOCOL_MAX_VALUE;
    GhosttyRenderStateImageData keys[] = {
        GHOSTTY_RENDER_STATE_IMAGE_DATA_PROTOCOL,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_ID,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_GENERATION,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_LAYER,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_Z,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_COL,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_ROW,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_X,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_Y,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_WIDTH,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_HEIGHT,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_X,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_Y,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_WIDTH,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_HEIGHT,
    };
    void *values[] = {
        &protocol,        &p->image_id,    &p->generation,
        &p->layer,       &p->z,           &p->col,
        &p->row,         &p->offset_x,     &p->offset_y,
        &p->width,       &p->height,       &p->source_x,
        &p->source_y,    &p->source_width, &p->source_height,
    };
    if (ghostty_render_state_image_get_multi(
            g_images_iter, sizeof(keys) / sizeof(keys[0]), keys, values,
            NULL) != GHOSTTY_SUCCESS ||
        protocol != GHOSTTY_IMAGE_PROTOCOL_KITTY) {
      continue;
    }
    ++count;
  }
  qsort(placements, count, sizeof(*placements), placement_compare);
  *out_count = count;
  return placements;
}

static uint8_t *to_cairo_pixels(const uint8_t *source, uint32_t width,
                                uint32_t height, GhosttyKittyImageFormat format,
                                int *out_stride) {
  const int stride = cairo_format_stride_for_width(CAIRO_FORMAT_ARGB32, width);
  if (stride <= 0 || (uint64_t)stride * height > SIZE_MAX) return NULL;
  uint8_t *result = calloc((size_t)stride, height);
  if (!result) return NULL;
  for (uint32_t y = 0; y < height; ++y) {
    uint8_t *row = result + (size_t)y * stride;
    for (uint32_t x = 0; x < width; ++x) {
      uint8_t r = 0, g = 0, b = 0, a = 255;
      if (format == GHOSTTY_KITTY_IMAGE_FORMAT_RGB) {
        const uint8_t *p = source + ((size_t)y * width + x) * 3;
        r = p[0]; g = p[1]; b = p[2];
      } else if (format == GHOSTTY_KITTY_IMAGE_FORMAT_RGBA) {
        const uint8_t *p = source + ((size_t)y * width + x) * 4;
        r = p[0]; g = p[1]; b = p[2]; a = p[3];
      } else if (format == GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA) {
        const uint8_t *p = source + ((size_t)y * width + x) * 2;
        r = g = b = p[0]; a = p[1];
      } else if (format == GHOSTTY_KITTY_IMAGE_FORMAT_GRAY) {
        r = g = b = source[(size_t)y * width + x];
      } else {
        free(result);
        return NULL;
      }
      const uint16_t alpha = a;
      row[x * 4 + 0] = (uint8_t)((b * alpha + 127) / 255);
      row[x * 4 + 1] = (uint8_t)((g * alpha + 127) / 255);
      row[x * 4 + 2] = (uint8_t)((r * alpha + 127) / 255);
      row[x * 4 + 3] = a;
    }
  }
  *out_stride = stride;
  return result;
}

static void draw_image_layer(cairo_t *cr, const ImagePlacement *placements,
                             size_t count, GhosttyImageLayer wanted,
                             GhosttyKittyGraphics graphics) {
  for (size_t index = 0; index < count; ++index) {
    const ImagePlacement *p = &placements[index];
    if (p->layer != wanted || p->width == 0 || p->height == 0 ||
        p->source_width == 0 || p->source_height == 0) {
      continue;
    }
    GhosttyKittyGraphicsImage image =
        ghostty_kitty_graphics_image(graphics, p->image_id);
    if (!image) continue;
    uint32_t image_width = 0, image_height = 0;
    GhosttyKittyImageFormat format = GHOSTTY_KITTY_IMAGE_FORMAT_RGBA;
    GhosttyString pixels = {0};
    GhosttyKittyGraphicsImageData keys[] = {
        GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_WIDTH,
        GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_HEIGHT,
        GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_FORMAT,
        GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_PIXELS,
    };
    void *values[] = {&image_width, &image_height, &format, &pixels};
    if (ghostty_kitty_graphics_image_get_multi(
            image, 4, keys, values, NULL) != GHOSTTY_SUCCESS ||
        !pixels.ptr || image_width == 0 || image_height == 0 ||
        (uint64_t)image_width * image_height > MAX_IMAGE_PIXELS) {
      continue;
    }
    int stride = 0;
    uint8_t *converted =
        to_cairo_pixels(pixels.ptr, image_width, image_height, format, &stride);
    if (!converted) continue;
    cairo_surface_t *surface = cairo_image_surface_create_for_data(
        converted, CAIRO_FORMAT_ARGB32, image_width, image_height, stride);
    if (cairo_surface_status(surface) != CAIRO_STATUS_SUCCESS) {
      cairo_surface_destroy(surface);
      free(converted);
      continue;
    }

    const double dest_x = p->col * (double)g_cell_width + p->offset_x;
    const double dest_y = p->row * (double)g_cell_height + p->offset_y;
    cairo_save(cr);
    cairo_rectangle(cr, 0, 0, g_cols * g_cell_width, g_rows * g_cell_height);
    cairo_clip(cr);
    cairo_rectangle(cr, dest_x, dest_y, p->width, p->height);
    cairo_clip(cr);
    cairo_translate(cr, dest_x, dest_y);
    cairo_scale(cr, p->width / (double)p->source_width,
                p->height / (double)p->source_height);
    cairo_set_source_surface(cr, surface, -(double)p->source_x,
                             -(double)p->source_y);
    cairo_pattern_set_filter(cairo_get_source(cr), CAIRO_FILTER_BILINEAR);
    cairo_pattern_set_extend(cairo_get_source(cr), CAIRO_EXTEND_PAD);
    cairo_paint(cr);
    cairo_restore(cr);
    cairo_surface_destroy(surface);
    free(converted);
  }
}

static void draw_cursor(cairo_t *cr, const GhosttyRenderStateColors *colors) {
  GhosttyRenderStateCursor cursor;
  memset(&cursor, 0, sizeof(cursor));
  cursor.size = sizeof(cursor);
  if (ghostty_render_state_get(g_render_state,
                               GHOSTTY_RENDER_STATE_DATA_CURSOR,
                               &cursor) != GHOSTTY_SUCCESS ||
      !cursor.visible || !cursor.viewport_has_value) {
    return;
  }
  GhosttyColorRgb color =
      colors->cursor_has_value ? colors->cursor : colors->foreground;
  set_rgb(cr, color, 0.85);
  const double x = cursor.viewport_x * g_cell_width;
  const double y = cursor.viewport_y * g_cell_height;
  if (cursor.visual_style == GHOSTTY_CURSOR_STYLE_BAR) {
    cairo_rectangle(cr, x, y, 2, g_cell_height);
  } else if (cursor.visual_style == GHOSTTY_CURSOR_STYLE_UNDERLINE) {
    cairo_rectangle(cr, x, y + g_cell_height - 2, g_cell_width, 2);
  } else {
    cairo_rectangle(cr, x, y, g_cell_width, g_cell_height);
  }
  cairo_fill(cr);
}

static void advance_animations(uint64_t now_ms, bool force_tick) {
  GhosttyKittyGraphics graphics = NULL;
  if (ghostty_terminal_get(g_terminal, GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
                           &graphics) != GHOSTTY_SUCCESS || !graphics) {
    return;
  }

  if (!g_animation_clock_initialized || now_ms < g_animation_time_ms) {
    uint64_t next_ms = 0;
    ghostty_kitty_graphics_animation_tick(graphics, now_ms, &next_ms);
    g_animation_clock_initialized = true;
    g_animation_time_ms = now_ms;
    return;
  }

  /* Ghostty advances at most one animation frame per tick. Reconstruct the
   * same 120 Hz clock for random seeks rather than jumping straight to the
   * destination and anchoring or advancing only one frame there. */
  while (g_animation_time_ms + 8 < now_ms) {
    g_animation_time_ms += 8;
    uint64_t next_ms = 0;
    ghostty_kitty_graphics_animation_tick(graphics, g_animation_time_ms,
                                          &next_ms);
  }
  if (force_tick || g_animation_time_ms < now_ms) {
    g_animation_time_ms = now_ms;
    uint64_t next_ms = 0;
    ghostty_kitty_graphics_animation_tick(graphics, g_animation_time_ms,
                                          &next_ms);
  }
}

static bool render_frame(uint64_t now_ms, ByteBuffer *png) {
  advance_animations(now_ms, false);
  GhosttyKittyGraphics graphics = NULL;
  ghostty_terminal_get(g_terminal, GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
                      &graphics);

  if (ghostty_render_state_update(g_render_state, g_terminal) !=
      GHOSTTY_SUCCESS) {
    return false;
  }
  GhosttyRenderStateColors colors;
  if (!get_colors(&colors)) return false;

  const int width = g_cols * (int)g_cell_width;
  const int height = g_rows * (int)g_cell_height;
  if (width <= 0 || height <= 0 ||
      (uint64_t)width * height > MAX_IMAGE_PIXELS) {
    return false;
  }
  cairo_surface_t *surface =
      cairo_image_surface_create(CAIRO_FORMAT_ARGB32, width, height);
  if (cairo_surface_status(surface) != CAIRO_STATUS_SUCCESS) {
    cairo_surface_destroy(surface);
    return false;
  }
  cairo_t *cr = cairo_create(surface);
  fill(cr, 0, 0, width, height, colors.background);

  size_t image_count = 0;
  ImagePlacement *placements = collect_images(&image_count);
  draw_image_layer(cr, placements, image_count,
                   GHOSTTY_IMAGE_LAYER_BELOW_BACKGROUND, graphics);
  draw_backgrounds(cr, &colors);
  draw_image_layer(cr, placements, image_count,
                   GHOSTTY_IMAGE_LAYER_BELOW_TEXT, graphics);
  draw_text(cr, &colors);
  draw_image_layer(cr, placements, image_count,
                   GHOSTTY_IMAGE_LAYER_ABOVE_TEXT, graphics);
  draw_cursor(cr, &colors);
  free(placements);

  cairo_destroy(cr);
  png->len = 0;
  cairo_status_t status =
      cairo_surface_write_to_png_stream(surface, png_write, png);
  cairo_surface_destroy(surface);
  ghostty_render_state_clean(g_render_state);
  return status == CAIRO_STATUS_SUCCESS;
}

static bool initialize(uint16_t cols, uint16_t rows, uint32_t cell_width,
                       uint32_t cell_height, double font_size) {
  g_cols = cols;
  g_rows = rows;
  g_cell_width = cell_width;
  g_cell_height = cell_height;
  g_font_size = font_size;
  if (ghostty_sys_set(GHOSTTY_SYS_OPT_DECODE_PNG,
                      (const void *)decode_png) != GHOSTTY_SUCCESS) {
    return false;
  }
  if (ghostty_terminal_new(NULL, &g_terminal, cols, rows) != GHOSTTY_SUCCESS)
    return false;
  ghostty_terminal_set(g_terminal, GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND,
                       &g_background);
  ghostty_terminal_set(g_terminal, GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND,
                       &g_foreground);
  if (g_palette_set)
    ghostty_terminal_set(g_terminal, GHOSTTY_TERMINAL_OPT_COLOR_PALETTE,
                         g_palette);
  if (ghostty_terminal_resize(g_terminal, cols, rows, cell_width, cell_height) !=
      GHOSTTY_SUCCESS)
    return false;
  if (ghostty_terminal_set(g_terminal, GHOSTTY_TERMINAL_OPT_WRITE_PTY,
                           (const void *)write_pty) != GHOSTTY_SUCCESS)
    return false;
  uint64_t image_limit = 256ull * 1024ull * 1024ull;
  ghostty_terminal_set(g_terminal,
                       GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT,
                       &image_limit);
  if (ghostty_render_state_new(NULL, &g_render_state) != GHOSTTY_SUCCESS)
    return false;
  if (ghostty_render_state_row_iterator_new(NULL, &g_rows_iter) !=
      GHOSTTY_SUCCESS)
    return false;
  if (ghostty_render_state_row_cells_new(NULL, &g_cells_iter) !=
      GHOSTTY_SUCCESS)
    return false;
  if (ghostty_render_state_image_iterator_new(NULL, &g_images_iter) !=
      GHOSTTY_SUCCESS)
    return false;
  return true;
}

static void cleanup(void) {
#ifdef __APPLE__
  release_faces();
#endif
  ghostty_render_state_image_iterator_free(g_images_iter);
  ghostty_render_state_row_cells_free(g_cells_iter);
  ghostty_render_state_row_iterator_free(g_rows_iter);
  ghostty_render_state_free(g_render_state);
  ghostty_terminal_free(g_terminal);
  ghostty_sys_set(GHOSTTY_SYS_OPT_DECODE_PNG, NULL);
}

static bool parse_color_arg(const char *value, GhosttyColorRgb *out) {
  return value && ghostty_color_parse(value, strlen(value), out) == GHOSTTY_SUCCESS;
}

static void parse_palette_arg(const char *value) {
  ghostty_color_palette_default(g_palette);
  if (!value || !*value) return;
  char *copy = strdup(value);
  if (!copy) return;
  char *save = NULL;
  char *token = strtok_r(copy, ",", &save);
  size_t index = 0;
  while (token && index < 16) {
    if (!parse_color_arg(token, &g_palette[index])) { free(copy); return; }
    index++;
    token = strtok_r(NULL, ",", &save);
  }
  g_palette_set = index == 16;
  free(copy);
}

int main(int argc, char **argv) {
  if (argc != 10) {
    fprintf(stderr,
            "usage: tap-vt-render COLS ROWS CELL_WIDTH CELL_HEIGHT FONT_SIZE BG FG PALETTE FONT_FAMILY\n");
    return 2;
  }
  long cols = strtol(argv[1], NULL, 10);
  long rows = strtol(argv[2], NULL, 10);
  long cell_width = strtol(argv[3], NULL, 10);
  long cell_height = strtol(argv[4], NULL, 10);
  double font_size = strtod(argv[5], NULL);
  if (!parse_color_arg(argv[6], &g_background) ||
      !parse_color_arg(argv[7], &g_foreground)) {
    fprintf(stderr, "invalid theme color\n");
    return 2;
  }
  parse_palette_arg(argv[8]);
  g_font_family = argv[9];
  if (cols <= 0 || cols > UINT16_MAX || rows <= 0 || rows > UINT16_MAX ||
      cell_width <= 0 || cell_width > UINT32_MAX || cell_height <= 0 ||
      cell_height > UINT32_MAX || font_size <= 0 ||
      !initialize((uint16_t)cols, (uint16_t)rows, (uint32_t)cell_width,
                  (uint32_t)cell_height, font_size)) {
    fprintf(stderr, "failed to initialize libghostty-vt renderer\n");
    cleanup();
    return 1;
  }
#ifdef __APPLE__
  load_faces();
#endif

  ByteBuffer frame = {0};
  bool running = true;
  while (running) {
    uint8_t header[5];
    if (!read_exact(header, sizeof(header))) break;
    const uint8_t operation = header[0];
    const uint32_t length = u32le(header + 1);
    if (length > MAX_REQUEST_BYTES) {
      respond(GHOSTTY_LIMIT_EXCEEDED, NULL, 0);
      break;
    }
    uint8_t *payload = NULL;
    if (length > 0) {
      payload = malloc(length);
      if (!payload || !read_exact(payload, length)) {
        free(payload);
        break;
      }
    }
    int32_t status = GHOSTTY_SUCCESS;
    switch (operation) {
    case OP_WRITE:
      g_replies.len = 0;
      ghostty_terminal_vt_write(g_terminal, payload, length);
      if (g_replies.len > UINT32_MAX)
        respond(GHOSTTY_LIMIT_EXCEEDED, NULL, 0);
      else
        respond(status, g_replies.data, (uint32_t)g_replies.len);
      break;
    case OP_RESIZE:
      if (length != 8) {
        status = GHOSTTY_INVALID_VALUE;
      } else {
        uint16_t next_cols = (uint16_t)u32le(payload);
        uint16_t next_rows = (uint16_t)u32le(payload + 4);
        status = ghostty_terminal_resize(g_terminal, next_cols, next_rows,
                                         g_cell_width, g_cell_height);
        if (status == GHOSTTY_SUCCESS) {
          g_cols = next_cols;
          g_rows = next_rows;
        }
      }
      respond(status, NULL, 0);
      break;
    case OP_FRAME:
      if (length != 8 || !render_frame(u64le(payload), &frame)) {
        status = GHOSTTY_INVALID_VALUE;
        respond(status, NULL, 0);
      } else if (frame.len > UINT32_MAX) {
        respond(GHOSTTY_LIMIT_EXCEEDED, NULL, 0);
      } else {
        respond(status, frame.data, (uint32_t)frame.len);
      }
      break;
    case OP_RESET:
      ghostty_terminal_reset(g_terminal);
      g_animation_clock_initialized = false;
      g_animation_time_ms = 0;
      respond(status, NULL, 0);
      break;
    case OP_SCROLL:
      if (length != 4) {
        status = GHOSTTY_INVALID_VALUE;
      } else {
        GhosttyTerminalScrollViewport scroll;
        memset(&scroll, 0, sizeof(scroll));
        scroll.tag = GHOSTTY_TERMINAL_SCROLL_VIEWPORT_DELTA;
        scroll.value.delta = i32le(payload);
        status = ghostty_terminal_scroll_viewport(g_terminal, scroll);
      }
      respond(status, NULL, 0);
      break;
    case OP_TICK:
      if (length != 8) {
        status = GHOSTTY_INVALID_VALUE;
      } else {
        advance_animations(u64le(payload), true);
      }
      respond(status, NULL, 0);
      break;
    case OP_QUIT:
      respond(status, NULL, 0);
      running = false;
      break;
    default:
      respond(GHOSTTY_INVALID_VALUE, NULL, 0);
      break;
    }
    free(payload);
  }
  free(frame.data);
  free(g_replies.data);
  cleanup();
  return 0;
}
