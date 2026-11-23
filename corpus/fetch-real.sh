#!/bin/sh
# Real-world test files from public repositories, for the real-world test
# (crates/libfileparser/tests/real_world.rs). Kept out of git: corpus/real/ is ignored,
# and each file keeps its repository's licence.
#
#   corpus/fetch-real.sh
#   LFP_REAL_CORPUS=$PWD/corpus/real cargo test -p libfileparser --test real_world -- --ignored --nocapture
#
# Each repository is cloned into a throwaway folder at its latest commit only
# (--depth 1), with only the folders that hold test files checked out
# (--filter=blob:none --sparse); its files are sorted into real/ by the reader each
# should open with, and the clone is deleted. Nothing but real/ is left behind.
set -e
cd "$(dirname "$0")"
rm -rf real real-repos
mkdir -p real
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fetch() {
  repo=$1; shift
  dir="$tmp/$(echo "$repo" | tr / _)"
  if git clone -q --depth 1 --filter=blob:none --sparse "https://github.com/$repo.git" "$dir" \
     && git -C "$dir" sparse-checkout set --no-cone "$@"; then
    python3 -I organize-real.py "$dir" real
  else
    echo "skipped $repo: couldn't clone it" >&2
  fi
  rm -rf "$dir"
}
# A repository's files with these extensions, wherever they are: its tree is listed
# (no file contents), up to 200 files of each extension are picked, and only those are
# checked out. For repositories whose test folders move.
fetch_ext() {
  repo=$1; shift
  dir="$tmp/$(echo "$repo" | tr / _)"
  if git clone -q --depth 1 --filter=blob:none --no-checkout "https://github.com/$repo.git" "$dir"; then
    list="$tmp/list"
    : > "$list"
    for ext in "$@"; do
      git -C "$dir" ls-tree -r --name-only HEAD | grep -i "\.$ext\$" | head -200 | sed 's|^|/|' >> "$list" || true
    done
    if [ -s "$list" ] && git -C "$dir" sparse-checkout set --no-cone --stdin < "$list" && git -C "$dir" checkout -q; then
      python3 -I organize-real.py "$dir" real
    else
      echo "skipped $repo: nothing with $*" >&2
    fi
  else
    echo "skipped $repo: couldn't clone it" >&2
  fi
  rm -rf "$dir"
}
fetch hashicorp/hcl '/specsuite/tests/'
fetch kdl-org/kdl '/tests/test_cases/input/' '/examples/'
fetch dhall-lang/dhall-lang '/tests/parser/success/unit/*A.dhall'
fetch tweag/nickel '/examples/' '/core/tests/integration/inputs/records/'
fetch imsweb/x12-parser '/src/test/resources/'
fetch nerdocs/pydifact '/tests/data/'
fetch LinuxForHealth/hl7v2-fhir-converter '/src/test/resources/*.hl7'
fetch Efferent-Health/HL7-dotnetcore '/test/Sample-*.txt'
fetch jseutter/ofxparse '/tests/fixtures/'
fetch jemmyw/Qif '/spec/fixtures/'
fetch WoLpH/mt940 '/mt940_tests/'
fetch Arelle/Arelle '/docs/website/examples/' '/tests/resources/conformance_suites/HMRC/**/*.xbrl'
fetch pydot/pydot '/test/graphs/'
fetch collective/icalendar '/src/icalendar/tests/calendars/'
fetch eventable/vobject '/test_files/'
fetch samtools/htslib '/test/*.sam' '/test/*.vcf' '/test/*.fa' '/test/*.gff3' '/test/fastq/*.fq'
fetch tkrajina/gpxpy '/test_files/*.gpx'
fetch mapbox/togeojson '/test/data/*.kml' '/test/data/*.gpx'
fetch toml-lang/toml-test '/tests/valid/' '/tests/invalid/'
fetch nst/JSONTestSuite '/test_parsing/'
fetch maxogden/csv-spectrum '/csvs/'
fetch yaml/yaml-test-suite '/src/'
fetch w3c/rdf-tests '/rdf/rdf11/rdf-turtle/*.ttl' '/rdf/rdf11/rdf-n-quads/*.nq' '/rdf/rdf11/rdf-trig/*.trig' '/rdf/rdf11/rdf-xml/**/*.rdf'
fetch izimobil/polib '/tests/*.po' '/tests/*.pot' '/tests/*.mo'
fetch protocolbuffers/protobuf '/src/google/protobuf/*.proto'

# Documents (libfileparser-docs).
fetch github/markup '/test/markups/'
fetch py-pdf/sample-files '/**/*.pdf'
fetch python-openxml/python-docx '/tests/test_files/*.docx'
fetch apache/poi '/test-data/document/*.docx' '/test-data/document/*.rtf' '/test-data/document/*.doc' '/test-data/slideshow/*.pptx'
fetch eea/odfpy '/tests/examples/*.odt' '/tests/examples/*.ods'
fetch apache/poi '/test-data/slideshow/*.ppt'
fetch LibreOffice/core '/sd/qa/unit/data/ppt/' '/sd/qa/unit/data/odp/' '/sd/qa/unit/data/pptx/'
fetch w3c/epubcheck '/src/test/resources/**/*.epub'
# Pandoc's tests: documents in many formats (GPL tests, kept out of git like the rest).
fetch jgm/pandoc '/test/'

# Markup, mail and books (libfileparser-docs, 2026-11-18). Pandoc's tests (above) give
# most markup formats a few files each.
fetch stalwartlabs/mail-parser '/resources/eml/' '/resources/mbox/'
fetch marirs/msg-parser-rs '/data/'
fetch matyushkin/djvu-rs '/tests/fixtures/' '/tests/corpus/'
fetch iyulab/unhwp '/tests/fixtures/'
fetch mete0r/pyhwp '/samples/' '/tests/**/*.hwp'
fetch jupyter/nbformat '/tests/'
fetch openbsd/src '/bin/*/*.1' '/usr.bin/c*/*.1'
fetch Perl/perl5 '/pod/perl[a-f]*.pod'
fetch typst/typst '/tests/suite/model/*.typ' '/docs/**/*.typ'
fetch asciidoctor/asciidoctor '/test/fixtures/*.adoc' '/docs/modules/**/*.adoc'
fetch nyousefi/Fountain '/**/*.fountain'
fetch bfabiszewski/libmobi '/tests/samples/'

# Spreadsheets (libfileparser-sheets).
fetch apache/poi '/test-data/spreadsheet/*.xlsx' '/test-data/spreadsheet/*.xlsm' '/test-data/spreadsheet/*.xlsb' '/test-data/spreadsheet/*.xls'
fetch pandas-dev/pandas '/pandas/tests/io/data/excel/'
fetch jmcnamara/XlsxWriter '/test/comparison/xlsx_files/'
fetch python-excel/xlrd '/tests/samples/'
fetch tafia/calamine '/tests/'
fetch masaccio/numbers-parser '/tests/data/*.numbers'
fetch LibreOffice/core '/sc/qa/unit/data/ods/' '/sc/qa/unit/data/xlsx/' '/sc/qa/unit/data/xls/' '/sc/qa/unit/data/fods/' '/sc/qa/unit/data/slk/' '/sc/qa/unit/data/dif/'

# Molecules and crystals, and signals (libfileparser-data, science). Open Babel's tests
# are GPL, the others BSD or MIT; kept out of git like the rest.
fetch openbabel/openbabel '/test/files/'
fetch rdkit/rdkit '/Code/GraphMol/FileParsers/test_data/*.mol' '/Code/GraphMol/FileParsers/test_data/*.sdf' '/Code/GraphMol/FileParsers/test_data/*.smi' '/Code/GraphMol/FileParsers/test_data/*.xyz'
fetch OpenChemistry/avogadrodata '/data/**/*.cjson' '/data/**/*.xyz' '/data/**/*.molden' '/data/**/*.sdf' '/data/**/*.mol'
fetch MolSSI/QCSchema '/tests/'
fetch cclib/cclib '/data/Turbomole/' '/data/Molden/' '/data/regression/Molden/'
fetch materialsproject/pymatgen '/test-files/**/POSCAR*' '/test-files/**/CONTCAR*' '/test-files/**/*.vasp' '!/test-files/**/*.gz' '!/test-files/**/*.bz2' '!/test-files/**/*.xz'
fetch gtkwave/gtkwave '/**/*.vcd'

# Images, archives and text (2026-11-22): images and their test suites (PngSuite among
# image-rs's), SVG from resvg's tests, archives broken and whole from zip2's and
# CPython's, tar-rs's and flate2's tests, and real text in dozens of languages and
# encodings from uchardet's.
fetch image-rs/image '/tests/images/'
fetch image-rs/image-tiff '/tests/images/'
fetch webmproject/libwebp-test-data '/*.webp'
fetch linebender/resvg '/crates/resvg/tests/**/*.svg'
fetch zip-rs/zip2 '/tests/data/'
fetch python/cpython '/Lib/test/archivetestdata/'
fetch alexcrichton/tar-rs '/tests/archives/'
fetch rust-lang/flate2-rs '/tests/'
fetch BYVoid/uchardet '/test/'

# Data formats that had no real files (2026-11-22): repositories listed by what they
# hold, not where, so a moved folder doesn't lose them (fetch_ext, above).
fetch_ext flutter/flutter arb
fetch_ext renatopp/arff-datasets arff
fetch_ext kurtmckee/feedparser xml atom rss
fetch_ext apache/avro avsc
fetch_ext arq5x/bedtools2 bed
fetch_ext capnproto/capnproto capnp
fetch_ext cue-lang/cue cue
fetch_ext holistics/dbml dbml
fetch_ext arminbiere/kissat cnf
fetch_ext motdotla/dotenv env
fetch_ext google/flatbuffers fbs
fetch_ext biopython/biopython gb gbk gff gtf
fetch_ext mapbox/togeojson geojson
fetch_ext topojson/topojson-client json topojson
fetch_ext gephi/gephi gexf
fetch_ext igraph/igraph graphml gml gxl
fetch_ext graphql/graphql-js graphql
fetch_ext hjson/hjson-js hjson
fetch_ext go-ini/ini ini
fetch_ext simdjson/simdjson ndjson jsonl
fetch_ext json5/json5-tests json5
fetch_ext w3c/json-ld-api jsonld
fetch_ext openldap/openldap ldif
fetch_ext logpai/loghub log
fetch_ext videojs/m3u8-parser m3u8 m3u
fetch_ext OAI/OpenAPI-Specification yaml json
fetch_ext osmcode/osm-testdata osm
fetch_ext sparkle-project/Sparkle plist strings
fetch_ext prisma/prisma-examples prisma
fetch_ext spring-projects/spring-boot properties
fetch_ext dotnet/winforms resx
fetch_ext smithy-lang/smithy smithy
fetch_ext w3c/rdf-tests rq ru
fetch_ext sqlfluff/sqlfluff sql
fetch_ext apache/thrift thrift
fetch_ext translate/translate tmx xlf xliff
fetch_ext RDFLib/rdflib trix
fetch_ext sabre-io/vobject vcf
fetch_ext georust/wkt wkt
fetch_ext lxml/lxml xml xsd
fetch_ext mitmproxy/mitmproxy har
# Audio and video (libfileparser-media): the browsers' shared tests hold real files in
# most containers and codecs.
fetch_ext web-platform-tests/wpt mp4 webm ogg ogv oga opus mp3 wav flac m4a mkv mov aac
fetch_ext michael-lazar/jetforce gmi

for d in real/*/; do printf '%-24s %s\n' "$(basename "$d")" "$(ls "$d" | wc -l | tr -d ' ')"; done
