#!/bin/sh
# Fetches the scrubber's entity model (GLiNER gliner_multi_pii-v1, ONNX, Apache-2.0 upstream) into a
# folder, pinned to one commit and checked against the hashes measured when it was adopted (ADR 0020).
# A file whose hash doesn't match is deleted and the script fails. About 1.2 GB.
#
#   scripts/fetch-pii-model.sh [folder]      default: models/gliner_multi_pii-v1
#   echo "Ask Imogen" | cargo run -p bullbar-cli -- scrub --model models/gliner_multi_pii-v1
set -eu

dir=${1:-models/gliner_multi_pii-v1}
base=https://huggingface.co/onnx-community/gliner_multi_pii-v1/resolve/2e0397a7e8a250d76c37122232b3cbde42c8d629

mkdir -p "$dir"
fetch() { # name, path in the repository, sha256
    if [ -f "$dir/$1" ] && echo "$3  $dir/$1" | sha256sum -c --status; then
        echo "$1: already here, hash matches"
        return
    fi
    echo "$1: downloading"
    curl -fL --proto '=https' -o "$dir/$1.part" "$base/$2"
    if echo "$3  $dir/$1.part" | sha256sum -c --status; then
        mv "$dir/$1.part" "$dir/$1"
        echo "$1: hash matches"
    else
        rm -f "$dir/$1.part"
        echo "$1: hash does NOT match; deleted. Nothing changed." >&2
        exit 1
    fi
}
fetch tokenizer.json tokenizer.json 914bd3c8fb7b525af9e23b60d0ec7b1248ddb2b99014efd9c02ebeb022f8cab7
fetch model.onnx onnx/model.onnx 7704865e414f24591da6aee08716a25677855bc8a84af81396d90e40df1e68d2
echo "Model ready in $dir"
