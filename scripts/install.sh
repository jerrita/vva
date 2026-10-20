#!/usr/bin/env bash
# 安装或更新 vva 到最新 release。环境变量 VVA_INSTALL_DIR 指定安装目录 (默认 ~/.local/bin)。
set -euo pipefail

repo="jerrita/vva"
dir="${VVA_INSTALL_DIR:-$HOME/.local/bin}"

os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
  Linux/x86_64) platform=linux-x64 ;;
  Linux/aarch64 | Linux/arm64) platform=linux-arm64 ;;
  Darwin/arm64) platform=macos-arm64 ;;
  *) echo "不支持的平台: $os/$arch" >&2; exit 1 ;;
esac

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
