#!/bin/sh
# Real-world test files from public repositories, for the real-world test
# (crates/libfileparser/tests/real_world.rs). Kept out of git: corpus/real/ is ignored,
# and each repository keeps its own licence.
#
#   corpus/fetch-real.sh
#   LFP_REAL_CORPUS=corpus/real cargo test -p libfileparser --test real_world -- --ignored --nocapture
#
# Each repository is cloned at its latest commit only (--depth 1), with only the
# folders that hold test files checked out (--filter=blob:none --sparse).
set -e
cd "$(dirname "$0")"
work=real-repos
mkdir -p "$work"
fetch() {
  repo=$1; shift
  dir="$work/$(echo "$repo" | tr / _)"
  [ -d "$dir" ] || git clone -q --depth 1 --filter=blob:none --sparse "https://github.com/$repo.git" "$dir"
  git -C "$dir" sparse-checkout set --no-cone "$@"
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
# Sorted into folders named by the reader each file should open with.
python3 -I organize-real.py "$work" real
