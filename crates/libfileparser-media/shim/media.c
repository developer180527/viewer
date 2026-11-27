// The part of FFmpeg libfileparser-media uses, as a handful of plain C calls: Rust
// never sees FFmpeg's structs, whose layout changes between releases.
//
// A file is read through callbacks into Rust (read, seek), so a memory-mapped file
// and one read by position work alike and nothing is copied whole. Long work (a
// frame, a waveform) asks `stop` between packets, through FFmpeg's interrupt callback.

#include <string.h>
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/channel_layout.h>
#include <libavutil/display.h>
#include <libavutil/imgutils.h>
#include <libavutil/opt.h>
#include <libswresample/swresample.h>
#include <libswscale/swscale.h>

typedef int (*vm_read_fn)(void *opaque, uint8_t *buf, int size);
typedef int64_t (*vm_seek_fn)(void *opaque, int64_t offset, int whence);
typedef int (*vm_stop_fn)(void *opaque);

typedef struct vm_media {
    AVFormatContext *fmt;
    AVIOContext *io;
    void *opaque;
    vm_read_fn read;
    vm_seek_fn seek;
    vm_stop_fn stop;
} vm_media;

enum { VM_VIDEO = 0, VM_AUDIO = 1, VM_SUBTITLE = 2, VM_DATA = 3, VM_ATTACHMENT = 4, VM_OTHER = 5 };

typedef struct vm_stream {
    int kind;
    char codec[64];
    char codec_long[160];
    char profile[64];
    char pix_fmt[48];
    char sample_fmt[48];
    char layout[96];
    int width, height;
    int fps_num, fps_den;
    int sample_rate, channels;
    int64_t bit_rate;
    int64_t duration_us;
    int64_t frames;
    int attached_pic;
    int is_default;
    int rotation;
} vm_stream;

static void copy(char *dst, size_t n, const char *src) {
    if (!src) src = "";
    strncpy(dst, src, n - 1);
    dst[n - 1] = 0;
}

static int io_read(void *o, uint8_t *buf, int size) {
    vm_media *m = o;
    int n = m->read(m->opaque, buf, size);
    return n <= 0 ? AVERROR_EOF : n;
}

static int64_t io_seek(void *o, int64_t offset, int whence) {
    vm_media *m = o;
    return m->seek(m->opaque, offset, whence);
}

static int io_stop(void *o) {
    vm_media *m = o;
    return m->stop ? m->stop(m->opaque) : 0;
}

// How sure FFmpeg is that these bytes start a file it can demux (0–100), and the
// demuxer's name. Only the header is looked at.
// FFmpeg reports what it meets in a damaged file on stderr; the reader says it instead.
static void quiet(void) { av_log_set_level(AV_LOG_QUIET); }

int vm_probe(const uint8_t *data, int len, const char *name, char *demuxer, int demuxer_len) {
    quiet();
    // FFmpeg's probe wants zeroed padding after the data.
    uint8_t *padded = av_mallocz((size_t)len + AVPROBE_PADDING_SIZE);
    if (!padded) return 0;
    memcpy(padded, data, (size_t)len);
    AVProbeData pd = { .filename = name ? name : "", .buf = padded, .buf_size = len };
    int score = 0;
    const AVInputFormat *f = av_probe_input_format3(&pd, 1, &score);
    av_free(padded);
    copy(demuxer, (size_t)demuxer_len, f ? f->name : "");
    return f ? score : 0;
}

vm_media *vm_open(void *opaque, vm_read_fn read, vm_seek_fn seek, vm_stop_fn stop, char *err, int errlen) {
    quiet();
    vm_media *m = av_mallocz(sizeof *m);
    if (!m) return NULL;
    m->opaque = opaque;
    m->read = read;
    m->seek = seek;
    m->stop = stop;
    const int bufsize = 1 << 16;
    uint8_t *buf = av_malloc(bufsize);
    m->io = avio_alloc_context(buf, bufsize, 0, m, io_read, NULL, io_seek);
    m->fmt = avformat_alloc_context();
    if (!m->io || !m->fmt) goto fail;
    m->fmt->pb = m->io;
    m->fmt->interrupt_callback.callback = io_stop;
    m->fmt->interrupt_callback.opaque = m;
    int r = avformat_open_input(&m->fmt, NULL, NULL, NULL);
    if (r < 0) {
        av_strerror(r, err, (size_t)errlen);
        m->fmt = NULL;
        goto fail;
    }
    r = avformat_find_stream_info(m->fmt, NULL);
    if (r < 0) {
        av_strerror(r, err, (size_t)errlen);
        goto fail;
    }
    return m;
fail:
    if (m->fmt) avformat_close_input(&m->fmt);
    if (m->io) {
        av_freep(&m->io->buffer);
        avio_context_free(&m->io);
    } else {
        av_free(buf);
    }
    av_free(m);
    return NULL;
}

void vm_close(vm_media *m) {
    if (!m) return;
    avformat_close_input(&m->fmt);
    if (m->io) {
        av_freep(&m->io->buffer);
        avio_context_free(&m->io);
    }
    av_free(m);
}

const char *vm_format_name(vm_media *m) { return m->fmt->iformat->name; }
const char *vm_format_long_name(vm_media *m) { return m->fmt->iformat->long_name ? m->fmt->iformat->long_name : ""; }
int64_t vm_duration_us(vm_media *m) { return m->fmt->duration == AV_NOPTS_VALUE ? -1 : m->fmt->duration; }
int64_t vm_bit_rate(vm_media *m) { return m->fmt->bit_rate; }
int vm_stream_count(vm_media *m) { return (int)m->fmt->nb_streams; }

int vm_stream_info(vm_media *m, int i, vm_stream *out) {
    if (i < 0 || (unsigned)i >= m->fmt->nb_streams) return -1;
    memset(out, 0, sizeof *out);
    AVStream *s = m->fmt->streams[i];
    AVCodecParameters *p = s->codecpar;
    switch (p->codec_type) {
    case AVMEDIA_TYPE_VIDEO: out->kind = VM_VIDEO; break;
    case AVMEDIA_TYPE_AUDIO: out->kind = VM_AUDIO; break;
    case AVMEDIA_TYPE_SUBTITLE: out->kind = VM_SUBTITLE; break;
    case AVMEDIA_TYPE_DATA: out->kind = VM_DATA; break;
    case AVMEDIA_TYPE_ATTACHMENT: out->kind = VM_ATTACHMENT; break;
    default: out->kind = VM_OTHER;
    }
    const AVCodecDescriptor *d = avcodec_descriptor_get(p->codec_id);
    copy(out->codec, sizeof out->codec, d ? d->name : avcodec_get_name(p->codec_id));
    copy(out->codec_long, sizeof out->codec_long, d && d->long_name ? d->long_name : "");
    copy(out->profile, sizeof out->profile, avcodec_profile_name(p->codec_id, p->profile));
    out->width = p->width;
    out->height = p->height;
    AVRational fps = s->avg_frame_rate.num ? s->avg_frame_rate : s->r_frame_rate;
    out->fps_num = fps.num;
    out->fps_den = fps.den;
    out->sample_rate = p->sample_rate;
    out->channels = p->ch_layout.nb_channels;
    av_channel_layout_describe(&p->ch_layout, out->layout, sizeof out->layout);
    if (p->codec_type == AVMEDIA_TYPE_VIDEO) copy(out->pix_fmt, sizeof out->pix_fmt, av_get_pix_fmt_name(p->format));
    if (p->codec_type == AVMEDIA_TYPE_AUDIO) copy(out->sample_fmt, sizeof out->sample_fmt, av_get_sample_fmt_name(p->format));
    out->bit_rate = p->bit_rate;
    out->duration_us = s->duration == AV_NOPTS_VALUE ? -1 : av_rescale_q(s->duration, s->time_base, AV_TIME_BASE_Q);
    out->frames = s->nb_frames;
    out->attached_pic = (s->disposition & AV_DISPOSITION_ATTACHED_PIC) != 0;
    out->is_default = (s->disposition & AV_DISPOSITION_DEFAULT) != 0;
    // Phones record upright video as landscape plus a rotation.
    const AVPacketSideData *sd = av_packet_side_data_get(p->coded_side_data, p->nb_coded_side_data, AV_PKT_DATA_DISPLAYMATRIX);
    if (sd && sd->size >= 9 * 4) {
        double r = av_display_rotation_get((const int32_t *)sd->data);
        out->rotation = (int)(r < 0 ? r - 0.5 : r + 0.5);
    }
    return 0;
}

static const AVDictionary *dict(vm_media *m, int stream) {
    if (stream < 0) return m->fmt->metadata;
    if ((unsigned)stream >= m->fmt->nb_streams) return NULL;
    return m->fmt->streams[stream]->metadata;
}

int vm_tag_count(vm_media *m, int stream) { return av_dict_count(dict(m, stream)); }

// Tag `index` of the file (stream -1) or of a stream.
int vm_tag(vm_media *m, int stream, int index, const char **key, const char **value) {
    const AVDictionaryEntry *e = NULL;
    for (int i = 0; (e = av_dict_iterate(dict(m, stream), e)); i++) {
        if (i == index) {
            *key = e->key;
            *value = e->value;
            return 0;
        }
    }
    return -1;
}

int vm_chapter_count(vm_media *m) { return (int)m->fmt->nb_chapters; }

int vm_chapter(vm_media *m, int i, int64_t *start_us, int64_t *end_us, const char **title) {
    if (i < 0 || (unsigned)i >= m->fmt->nb_chapters) return -1;
    AVChapter *c = m->fmt->chapters[i];
    *start_us = av_rescale_q(c->start, c->time_base, AV_TIME_BASE_Q);
    *end_us = av_rescale_q(c->end, c->time_base, AV_TIME_BASE_Q);
    const AVDictionaryEntry *t = av_dict_get(c->metadata, "title", NULL, 0);
    *title = t ? t->value : "";
    return 0;
}

// The cover art a music file carries (an attached picture), as its encoded bytes.
int vm_cover(vm_media *m, const uint8_t **data, int *len) {
    for (unsigned i = 0; i < m->fmt->nb_streams; i++) {
        AVStream *s = m->fmt->streams[i];
        if ((s->disposition & AV_DISPOSITION_ATTACHED_PIC) && s->attached_pic.size > 0) {
            *data = s->attached_pic.data;
            *len = s->attached_pic.size;
            return 0;
        }
    }
    return -1;
}

// `max_pixels`: the most a decoded picture may have. FFmpeg's default is INT_MAX, so
// a few bytes of crafted header could make a decoder allocate gigabytes of frames.
static AVCodecContext *open_decoder(AVStream *s, int64_t max_pixels) {
    const AVCodec *codec = avcodec_find_decoder(s->codecpar->codec_id);
    if (!codec) return NULL;
    AVCodecContext *c = avcodec_alloc_context3(codec);
    if (!c) return NULL;
    if (max_pixels > 0) c->max_pixels = max_pixels;
    if (avcodec_parameters_to_context(c, s->codecpar) < 0 || avcodec_open2(c, codec, NULL) < 0) {
        avcodec_free_context(&c);
        return NULL;
    }
    return c;
}

// Only stream `si`'s packets are read from here on: demuxers that can (MP4, MKV) skip
// the others' bytes instead of reading them, so a film's waveform doesn't read its video.
// Each reader calls this for its own stream, which also undoes another reader's choice.
static void only(vm_media *m, int si) {
    for (unsigned i = 0; i < m->fmt->nb_streams; i++) m->fmt->streams[i]->discard = (int)i == si ? AVDISCARD_DEFAULT : AVDISCARD_ALL;
}

// The best video stream's frame nearest `at_us` (the first frame after the keyframe
// before it), scaled to fit `max_side` and converted to RGBA, in a buffer to free
// with vm_free. 0 on success; a negative AVERROR otherwise.
int vm_frame(vm_media *m, int64_t at_us, int max_side, int64_t max_pixels, uint8_t **rgba, int *width, int *height) {
    int si = av_find_best_stream(m->fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    if (si < 0) return si;
    AVStream *s = m->fmt->streams[si];
    if (s->disposition & AV_DISPOSITION_ATTACHED_PIC) return AVERROR_STREAM_NOT_FOUND;
    AVCodecContext *c = open_decoder(s, max_pixels);
    if (!c) return AVERROR_DECODER_NOT_FOUND;
    only(m, si);
    if (at_us > 0) {
        int64_t ts = av_rescale_q(at_us, AV_TIME_BASE_Q, s->time_base);
        if (av_seek_frame(m->fmt, si, ts, AVSEEK_FLAG_BACKWARD) < 0) av_seek_frame(m->fmt, si, 0, AVSEEK_FLAG_BACKWARD);
    }
    AVPacket *pkt = av_packet_alloc();
    AVFrame *frame = av_frame_alloc();
    int r = AVERROR(EAGAIN), got = 0, packets = 0;
    while (!got && pkt && frame && (r = av_read_frame(m->fmt, pkt)) >= 0) {
        if (pkt->stream_index == si) {
            r = avcodec_send_packet(c, pkt);
            while (r >= 0 && !got) {
                r = avcodec_receive_frame(c, frame);
                if (r >= 0) got = 1;
            }
            // A stream that never yields a frame: stop rather than read to the end.
            if (++packets > 2000) break;
        }
        av_packet_unref(pkt);
        if (io_stop(m)) break;
    }
    if (!got && pkt && frame) {
        // Drain: short files hold their only frames in the decoder.
        avcodec_send_packet(c, NULL);
        if (avcodec_receive_frame(c, frame) >= 0) got = 1;
    }
    int result = AVERROR_INVALIDDATA;
    if (got && frame->width > 0 && frame->height > 0) {
        double k = (double)max_side / (frame->width > frame->height ? frame->width : frame->height);
        if (k > 1.0) k = 1.0;
        int w = (int)(frame->width * k + 0.5), h = (int)(frame->height * k + 0.5);
        if (w < 1) w = 1;
        if (h < 1) h = 1;
        struct SwsContext *sws = sws_getContext(frame->width, frame->height, frame->format, w, h, AV_PIX_FMT_RGBA, SWS_BICUBIC, NULL, NULL, NULL);
        uint8_t *buf = av_malloc((size_t)w * h * 4);
        if (sws && buf) {
            uint8_t *dst[4] = { buf };
            int stride[4] = { w * 4 };
            sws_scale(sws, (const uint8_t *const *)frame->data, frame->linesize, 0, frame->height, dst, stride);
            *rgba = buf;
            *width = w;
            *height = h;
            result = 0;
        } else {
            av_free(buf);
            result = AVERROR(ENOMEM);
        }
        sws_freeContext(sws);
    } else if (r < 0 && r != AVERROR_EOF) {
        result = r;
    }
    av_frame_free(&frame);
    av_packet_free(&pkt);
    avcodec_free_context(&c);
    return result;
}

typedef struct peak_state {
    float *out;
    int buckets;
    double total;
    int64_t done;
    int reached;
} peak_state;

static void take(peak_state *st, const float *v, int n) {
    for (int k = 0; k < n && st->done < st->total; k++, st->done++) {
        int b = (int)(st->done * st->buckets / st->total);
        float a = v[k] < 0 ? -v[k] : v[k];
        if (a > st->out[b]) st->out[b] = a;
        if (b + 1 > st->reached) st->reached = b + 1;
    }
}

// The resampler to mono at `rate`, and the input it was set up for. It's set up from
// each frame, not from the stream's header: a damaged file can decode frames with
// another channel count or sample format than its header says, and a resampler fed
// planes it wasn't set up for reads past them.
typedef struct mono {
    SwrContext *swr;
    int rate;
    int fmt, in_rate;
    AVChannelLayout layout;
} mono;

static void mono_free(mono *m) {
    swr_free(&m->swr);
    av_channel_layout_uninit(&m->layout);
}

// Ready for `frame`: the same resampler when the frame's format matches, else a new
// one (what the old one held is drained into the peaks first). 0 when it can't be.
static int mono_for(mono *m, const AVFrame *frame, peak_state *st);

// A decoded frame through the resampler into the peaks; NULL flushes what it holds.
static void resample(mono *m, AVFrame *frame, peak_state *st) {
    if (frame && (!frame->extended_data || frame->nb_samples <= 0 || !mono_for(m, frame, st))) return;
    if (!m->swr) return;
    float tmp[4096];
    uint8_t *o[1] = { (uint8_t *)tmp };
    int n = swr_convert(m->swr, o, 4096, frame ? (const uint8_t **)frame->extended_data : NULL, frame ? frame->nb_samples : 0);
    if (n > 0) take(st, tmp, n);
    // More than one buffer's worth came out of that frame, or a flush: drain it all.
    while ((frame ? swr_get_out_samples(m->swr, 0) >= 4096 : 1) && (n = swr_convert(m->swr, o, 4096, NULL, 0)) > 0) take(st, tmp, n);
}

static int mono_for(mono *m, const AVFrame *frame, peak_state *st) {
    // Real audio stays under 1 MHz (DSD decodes to 352.8 kHz). A damaged header's
    // 654 MHz made swresample spend ten seconds building its filter.
    if (frame->ch_layout.nb_channels <= 0 || frame->sample_rate <= 0 || frame->sample_rate > 1000000 || frame->format < 0) return 0;
    if (m->swr && frame->format == m->fmt && frame->sample_rate == m->in_rate && !av_channel_layout_compare(&frame->ch_layout, &m->layout)) return 1;
    if (m->swr) resample(m, NULL, st);
    mono_free(m);
    AVChannelLayout out = AV_CHANNEL_LAYOUT_MONO;
    if (swr_alloc_set_opts2(&m->swr, &out, AV_SAMPLE_FMT_FLT, m->rate, &frame->ch_layout, frame->format, frame->sample_rate, 0, NULL) < 0 || swr_init(m->swr) < 0 ||
        av_channel_layout_copy(&m->layout, &frame->ch_layout) < 0) {
        mono_free(m);
        return 0;
    }
    m->fmt = frame->format;
    m->in_rate = frame->sample_rate;
    return 1;
}

// The best audio stream decoded to mono, as the peak level of each of `buckets`
// stretches of its length (0–1). `max_seconds` stops a very long file early (0: all).
// Returns how many buckets were reached, or a negative AVERROR.
int vm_peaks(vm_media *m, int buckets, float *out, double max_seconds) {
    if (buckets <= 0 || !out) return AVERROR(EINVAL);
    int si = av_find_best_stream(m->fmt, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    if (si < 0) return si;
    AVStream *s = m->fmt->streams[si];
    AVCodecContext *c = open_decoder(s, 0);
    if (!c) return AVERROR_DECODER_NOT_FOUND;
    only(m, si);
    double seconds = s->duration != AV_NOPTS_VALUE ? s->duration * av_q2d(s->time_base) : m->fmt->duration != AV_NOPTS_VALUE ? m->fmt->duration / (double)AV_TIME_BASE : 0;
    if (max_seconds > 0 && (seconds <= 0 || seconds > max_seconds)) seconds = max_seconds;
    if (seconds <= 0) seconds = 600;
    const int rate = 8000; // enough to see a waveform's shape
    double total = seconds * rate;
    mono mono = { .rate = rate };
    for (int i = 0; i < buckets; i++) out[i] = 0;
    // No seek to the start: a file is opened afresh for this, and FFmpeg replays the
    // packets it read while probing. Seeking would drop them, and a small raw file
    // (FLAC without a seek table) would then read nothing.
    AVPacket *pkt = av_packet_alloc();
    AVFrame *frame = av_frame_alloc();
    peak_state st = { out, buckets, total, 0, 0 };
    int r = 0;
    while (pkt && frame && st.done < total && (r = av_read_frame(m->fmt, pkt)) >= 0) {
        if (pkt->stream_index == si && avcodec_send_packet(c, pkt) >= 0) {
            while (avcodec_receive_frame(c, frame) >= 0) resample(&mono, frame, &st);
        }
        av_packet_unref(pkt);
        if (io_stop(m)) break;
    }
    // The decoder's last frames, then what the resampler still holds: a very short
    // file is all in these.
    if (pkt && frame && avcodec_send_packet(c, NULL) >= 0) {
        while (avcodec_receive_frame(c, frame) >= 0) resample(&mono, frame, &st);
    }
    resample(&mono, NULL, &st);
    int reached = st.reached;
    av_frame_free(&frame);
    av_packet_free(&pkt);
    mono_free(&mono);
    avcodec_free_context(&c);
    for (int i = 0; i < buckets; i++) {
        if (out[i] > 1) out[i] = 1;
    }
    return reached;
}

void vm_free(void *p) { av_free(p); }
