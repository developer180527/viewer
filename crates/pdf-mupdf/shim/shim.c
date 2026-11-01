/* The only C Viewer calls in MuPDF. MuPDF reports errors with setjmp/longjmp
 * (fz_try / fz_catch), which must never cross into Rust, so every entry point here
 * catches them and returns 0 on success or -1 with the message copied into `err`.
 *
 * One vw_doc owns its own fz_context and is never used by two threads at once (the
 * Rust side holds it behind a mutex), so no locks are given to MuPDF. */

#include <mupdf/fitz.h>
#include <string.h>

typedef struct vw_doc {
	fz_context *ctx;
	fz_stream *stm;
	fz_document *doc;
} vw_doc;

static void set_err(char *err, size_t len, const char *msg) {
	if (!err || len == 0) return;
	strncpy(err, msg ? msg : "unknown error", len - 1);
	err[len - 1] = 0;
}

/* Copies the caught error's message into `err` and marks it handled (MuPDF warns
 * about a caught error that is neither reported nor ignored). */
static void caught(fz_context *ctx, char *err, size_t len) {
	set_err(err, len, fz_caught_message(ctx));
	fz_ignore_error(ctx);
}

/* MuPDF prints warnings and errors to stderr by default; errors reach Rust as
 * messages instead, and warnings (repairs of damaged files) aren't shown. */
static void quiet(void *user, const char *message) {
	(void)user;
	(void)message;
}

/* Opens `data` (which must outlive the vw_doc) as a document of type `magic` (a
 * MIME type or file name). `store` caps MuPDF's cache of decoded fonts and images. */
vw_doc *vw_open(const unsigned char *data, size_t len, const char *magic, size_t store, char *err, size_t err_len) {
	vw_doc *d;
	fz_context *ctx = fz_new_context(NULL, NULL, store);
	if (!ctx) {
		set_err(err, err_len, "out of memory");
		return NULL;
	}
	d = calloc(1, sizeof *d);
	if (!d) {
		fz_drop_context(ctx);
		set_err(err, err_len, "out of memory");
		return NULL;
	}
	d->ctx = ctx;
	fz_set_warning_callback(ctx, quiet, NULL);
	fz_set_error_callback(ctx, quiet, NULL);
	fz_try(ctx) {
		fz_register_document_handlers(ctx);
		d->stm = fz_open_memory(ctx, data, len);
		d->doc = fz_open_document_with_stream(ctx, magic, d->stm);
	}
	fz_catch(ctx) {
		caught(ctx, err, err_len);
		fz_drop_stream(ctx, d->stm);
		fz_drop_context(ctx);
		free(d);
		return NULL;
	}
	return d;
}

void vw_close(vw_doc *d) {
	if (!d) return;
	fz_drop_document(d->ctx, d->doc);
	fz_drop_stream(d->ctx, d->stm);
	fz_drop_context(d->ctx);
	free(d);
}

/* 1 if the document needs a password, 0 if not. */
int vw_needs_password(vw_doc *d) {
	int r = 0;
	fz_try(d->ctx) r = fz_needs_password(d->ctx, d->doc);
	fz_catch(d->ctx) {
		fz_ignore_error(d->ctx);
		r = 0;
	}
	return r;
}

int vw_page_count(vw_doc *d, int *count, char *err, size_t err_len) {
	fz_try(d->ctx) *count = fz_count_pages(d->ctx, d->doc);
	fz_catch(d->ctx) {
		caught(d->ctx, err, err_len);
		return -1;
	}
	return 0;
}

/* The page's bounds in points as shown (rotation applied): x0, y0, x1, y1. */
int vw_page_bounds(vw_doc *d, int index, float out[4], char *err, size_t err_len) {
	fz_page *page = NULL;
	fz_var(page);
	fz_try(d->ctx) {
		fz_rect r;
		page = fz_load_page(d->ctx, d->doc, index);
		r = fz_bound_page(d->ctx, page);
		out[0] = r.x0; out[1] = r.y0; out[2] = r.x1; out[3] = r.y1;
	}
	fz_always(d->ctx) fz_drop_page(d->ctx, page);
	fz_catch(d->ctx) {
		caught(d->ctx, err, err_len);
		return -1;
	}
	return 0;
}

/* Renders the region (x, y) .. (x + w/scale, y + h/scale) of page `index`, in
 * points from the page's bounds' top left, into `rgba` (w * h * 4 bytes, opaque,
 * on white). */
int vw_render(vw_doc *d, int index, float scale, float x, float y, int w, int h, unsigned char *rgba, char *err, size_t err_len) {
	fz_page *page = NULL;
	fz_pixmap *pix = NULL;
	fz_device *dev = NULL;
	fz_var(page);
	fz_var(pix);
	fz_var(dev);
	fz_try(d->ctx) {
		fz_rect b;
		fz_matrix ctm;
		page = fz_load_page(d->ctx, d->doc, index);
		b = fz_bound_page(d->ctx, page);
		pix = fz_new_pixmap_with_data(d->ctx, fz_device_rgb(d->ctx), w, h, NULL, 1, w * 4, rgba);
		fz_clear_pixmap_with_value(d->ctx, pix, 255);
		/* Page space -> pixels: move the region's corner to the origin, then scale. */
		ctm = fz_concat(fz_translate(-(b.x0 + x), -(b.y0 + y)), fz_scale(scale, scale));
		dev = fz_new_draw_device(d->ctx, ctm, pix);
		fz_run_page(d->ctx, page, dev, fz_identity, NULL);
		fz_close_device(d->ctx, dev);
	}
	fz_always(d->ctx) {
		fz_drop_device(d->ctx, dev);
		fz_drop_pixmap(d->ctx, pix);
		fz_drop_page(d->ctx, page);
	}
	fz_catch(d->ctx) {
		caught(d->ctx, err, err_len);
		return -1;
	}
	return 0;
}

/* Copies metadata `key` (FZ_META_*, e.g. "format", "info:Title") into `buf`.
 * Returns the length, or -1 if there is none. */
int vw_metadata(vw_doc *d, const char *key, char *buf, size_t len) {
	int r = -1;
	fz_try(d->ctx) r = fz_lookup_metadata(d->ctx, d->doc, key, buf, len);
	fz_catch(d->ctx) {
		fz_ignore_error(d->ctx);
		r = -1;
	}
	return r;
}
