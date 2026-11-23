#!/bin/sh
# Writes the corpus's small audio and video files into corpus/files/media/ with the
# ffmpeg command-line tool (any build: it only encodes these, nothing links it). One
# second each, tiny frames, so the committed files stay a few KB. Rerun only to add or
# change one, then refresh corpus/expected/ (corpus/README.md).
#
#   corpus/make-media.sh
set -eu
cd "$(dirname "$0")"
out=files/media
mkdir -p "$out"
ff() { ffmpeg -hide_banner -loglevel error -y "$@"; }
# A moving test pattern and a tone, the same in every file.
video="testsrc2=size=96x64:rate=24:duration=1"
audio="sine=frequency=440:sample_rate=44100:duration=1"
# Uncompressed audio: smaller, so the PCM files stay small.
pcm="sine=frequency=440:sample_rate=8000:duration=1"
chapters="$(mktemp)"
trap 'rm -f "$chapters"' EXIT
printf ';FFMETADATA1\ntitle=Test pattern\nartist=Viewer corpus\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=First half\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Second half\n' > "$chapters"

# Video: H.264 and AAC in MP4, with tags and chapters, recorded "rotated" as phones do.
ff -f lavfi -i "$video" -f lavfi -i "$audio" -i "$chapters" -map 0 -map 1 -map_metadata 2 -map_chapters 2 \
  -c:v libx264 -preset veryfast -crf 35 -pix_fmt yuv420p -c:a aac -b:a 32k "$out/upright.mp4"
# The rotation goes on as the streams are copied (it's an input option).
ff -display_rotation:v:0 90 -i "$out/upright.mp4" -map 0 -map_chapters 0 -c copy "$out/h264-aac.mp4"
rm -f "$out/upright.mp4"
ff -f lavfi -i "$video" -c:v libx265 -preset fast -crf 38 -tag:v hvc1 -an "$out/hevc.mp4"
ff -f lavfi -i "$video" -f lavfi -i "$audio" -c:v libvpx-vp9 -b:v 50k -deadline realtime -c:a libopus -b:a 24k "$out/vp9-opus.webm"
ff -f lavfi -i "$video" -c:v libsvtav1 -crf 50 -preset 12 -an "$out/av1.mkv"
ff -f lavfi -i "$video" -c:v prores_ks -profile:v 0 -an "$out/prores.mov"
ff -f lavfi -i "testsrc2=size=96x64:rate=8:duration=1" -f lavfi -i "$pcm" -c:v mjpeg -q:v 20 -c:a pcm_s16le "$out/mjpeg-pcm.avi"
# Subtitles alongside: a stream that isn't audio or video.
printf '1\n00:00:00,000 --> 00:00:00,900\nHello\n' > "$chapters.srt"
ff -f lavfi -i "$video" -f lavfi -i "$audio" -i "$chapters.srt" -map 0 -map 1 -map 2 -c:v libx264 -preset veryfast -crf 35 -c:a aac -b:a 32k -c:s srt \
  -metadata:s:a:0 language=eng -metadata:s:s:0 language=fra "$out/subtitled.mkv"
rm -f "$chapters.srt"

# Audio: MP3 with ID3 tags and cover art, and the lossless and open codecs.
ff -f lavfi -i "color=c=orange:size=32x32" -frames:v 1 "$chapters.png"
ff -f lavfi -i "$audio" -i "$chapters.png" -map 0 -map 1 -c:a libmp3lame -b:a 32k -c:v copy \
  -id3v2_version 3 -metadata title="Tone" -metadata artist="Viewer corpus" -metadata album="Samples" -disposition:v attached_pic "$out/cover.mp3"
rm -f "$chapters.png"
ff -f lavfi -i "$audio" -c:a flac "$out/tone.flac"
ff -f lavfi -i "$pcm" -c:a pcm_s16le "$out/tone.wav"
ff -f lavfi -i "$audio" -c:a libopus -b:a 24k "$out/tone.opus"
ff -f lavfi -i "$audio" -c:a alac "$out/tone.m4a"
ff -f lavfi -i "$pcm" -c:a pcm_s16be "$out/tone.aiff"
ls -l "$out" | awk 'NR > 1 { print $5, $9 }'
