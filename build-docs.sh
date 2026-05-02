#!/bin/sh

set -eu

cargo install -q mdbook
mdbook build

# mdBook overwrites docs/, so every file GitHub Pages needs but mdBook does
# not generate must be copied after each build.
cp ./bootstrap.sh docs/bootstrap.sh
cp ./bootstrap.bat docs/bootstrap.bat
cp ./bootstrap.ps1 docs/bootstrap.ps1

if [ -f ./.nojekyll ]; then
    cp ./.nojekyll docs/.nojekyll
else
    : > docs/.nojekyll
fi

for archive in ./release-*.tar.gz ./target/release-*.tar.gz ./target/*/release-*.tar.gz; do
    if [ -f "$archive" ]; then
        cp "$archive" docs/
    fi
done
