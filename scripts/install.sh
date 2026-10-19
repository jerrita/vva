#!/usr/bin/env bash
# 安装或更新 vva 到最新 release。环境变量 VVA_INSTALL_DIR 指定安装目录 (默认 ~/.local/bin)。
set -euo pipefail

repo="jerrita/vva"
dir="${VVA_INSTALL_DIR:-$HOME/.local/bin}"

[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || {
  echo "仅支持 Linux x86_64" >&2
  exit 1
}

tag=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" |
  sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)
[ -n "$tag" ] || { echo "无法获取最新版本" >&2; exit 1; }

name="vva-$tag-linux-x64"
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
