#!/bin/sh
# Real-world test files from public repositories, for the real-world test
# (crates/libfileparser/tests/real_world.rs). Kept out of git: corpus/real/ is ignored,
# and each file keeps its repository's licence.
#
#   corpus/fetch-real.sh
#   LFP_REAL_CORPUS=corpus/real cargo test -p libfileparser --test real_world -- --ignored --nocapture
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
fetch w3c/epubcheck '/src/test/resources/**/*.epub'
# Pandoc's tests: documents in many formats (GPL tests, kept out of git like the rest).
fetch jgm/pandoc '/test/'

# Spreadsheets (libfileparser-sheets).
fetch apache/poi '/test-data/spreadsheet/*.xlsx' '/test-data/spreadsheet/*.xlsm' '/test-data/spreadsheet/*.xlsb' '/test-data/spreadsheet/*.xls'
fetch pandas-dev/pandas '/pandas/tests/io/data/excel/'
fetch jmcnamara/XlsxWriter '/test/comparison/xlsx_files/'
fetch python-excel/xlrd '/tests/samples/'
fetch tafia/calamine '/tests/'
fetch LibreOffice/core '/sc/qa/unit/data/ods/' '/sc/qa/unit/data/xlsx/' '/sc/qa/unit/data/xls/' '/sc/qa/unit/data/fods/' '/sc/qa/unit/data/slk/' '/sc/qa/unit/data/dif/'

for d in real/*/; do printf '%-24s %s\n' "$(basename "$d")" "$(ls "$d" | wc -l | tr -d ' ')"; done
