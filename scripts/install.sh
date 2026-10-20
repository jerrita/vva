#!/usr/bin/env bash
# 安装或更新 vva 到最新 release。VVA_INSTALL_DIR 指定安装目录，默认 ~/.local/bin，Termux 使用 $PREFIX/bin。
set -euo pipefail

repo="jerrita/vva"
os=$(uname -s)
arch=$(uname -m)
if [ "$os" = Linux ] && {
  [ -n "${TERMUX_VERSION:-}" ] ||
    [ "${PREFIX:-}" = /data/data/com.termux/files/usr ] ||
    [ "$(uname -o 2>/dev/null || true)" = Android ]
}; then
  os=Android
fi

case "$os/$arch" in
  Android/aarch64 | Android/arm64) platform=android-arm64 ;;
  Linux/x86_64) platform=linux-x64 ;;
  Linux/aarch64 | Linux/arm64) platform=linux-arm64 ;;
  Darwin/arm64) platform=macos-arm64 ;;
  *) echo "不支持的平台: $os/$arch" >&2; exit 1 ;;
esac

if [ "$platform" = android-arm64 ]; then
  dir="${VVA_INSTALL_DIR:-${PREFIX:-/data/data/com.termux/files/usr}/bin}"
else
  dir="${VVA_INSTALL_DIR:-$HOME/.local/bin}"
fi

tag=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" |
  sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)
[ -n "$tag" ] || { echo "无法获取最新版本" >&2; exit 1; }

name="vva-$tag-$platform"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "下载 $tag ..."
curl -fsSL "https://github.com/$repo/releases/download/$tag/$name.tar.gz" | tar xz -C "$tmp"
mkdir -p "$dir"
install -m 755 "$tmp/$name/vva" "$dir/vva"
echo "已安装 vva $tag 到 $dir/vva"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "请将 $dir 加入 PATH" ;;
esac
