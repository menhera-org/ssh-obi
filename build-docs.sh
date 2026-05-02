#!/bin/sh

set -eu

cargo install -q mdbook
mdbook build
printf %s obi.menhera.org > docs/CNAME

