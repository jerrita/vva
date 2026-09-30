//! 工程文件模糊查找（"go to file"）：纯打分函数与工作区遍历。

use std::path::Path;
use std::time::SystemTime;

use ignore::WalkBuilder;

/// 单次遍历访问条目上限，避免超大工作区拖垮服务
pub const MAX_VISITED: usize = 50_000;

/// 一次模糊匹配结果：分数（越大越好）与命中的字符下标（按 `path` 的 char 序号）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i64,
    pub indices: Vec<usize>,
}

const BASE: i64 = 10;
const BONUS_SEGMENT_START: i64 = 16;
const BONUS_CONSECUTIVE: i64 = 12;
const BONUS_IN_NAME: i64 = 8;
const PENALTY_GAP: i64 = 3;

fn is_separator(c: char) -> bool {
    matches!(c, '/' | '_' | '-' | '.' | ' ')
}

/// `i` 位置是否处于段首（路径起点 / 分隔符之后 / camelCase 大写边界）
fn is_segment_start(chars: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let (prev, cur) = (chars[i - 1], chars[i]);
    is_separator(prev) || (prev.is_lowercase() && cur.is_uppercase())
}

/// 不区分大小写的模糊子序列匹配；未命中返回 `None`。
///
/// 通过动态规划寻找全局最优对齐：文件名内命中、连续命中、段首命中加分，
/// 命中间断与路径较长扣分。
pub fn fuzzy_match(query: &str, path: &str) -> Option<FuzzyMatch> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: Vec::new(),
        });
    }
    let chars: Vec<char> = path.chars().collect();
    let lower: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let (n, m) = (chars.len(), q.len());
    if m > n {
        return None;
    }
    let name_start = chars.iter().rposition(|&c| c == '/').map_or(0, |p| p + 1);

    let bonus = |i: usize| -> i64 {
        let mut b = BASE;
        if is_segment_start(&chars, i) {
            b += BONUS_SEGMENT_START;
        }
        if i >= name_start {
            b += BONUS_IN_NAME;
        }
        b
    };

    // dp[j][i]：q[j] 匹配在 path[i] 时的最优得分及前驱位置
    const NEG: i64 = i64::MIN / 2;
    let mut dp = vec![vec![(NEG, usize::MAX); n]; m];
    for j in 0..m {
        // best_prev：满足 k <= i-2 的 dp[j-1][k] 最优（含位置）
        let mut best_prev = (NEG, usize::MAX);
        for i in j..n {
            if j > 0 && i >= 2 && i - 2 >= j - 1 {
                let cand = dp[j - 1][i - 2];
                if cand.0 > best_prev.0 {
                    best_prev = (cand.0, i - 2);
                }
            }
            if lower[i] != q[j] {
                continue;
            }
            let b = bonus(i);
            if j == 0 {
                dp[0][i] = (b, usize::MAX);
                continue;
            }
            let mut best = (NEG, usize::MAX);
            if i >= 1 && dp[j - 1][i - 1].0 > NEG {
                best = (dp[j - 1][i - 1].0 + BONUS_CONSECUTIVE, i - 1);
            }
            if best_prev.0 > NEG {
                let gapped = best_prev.0 - PENALTY_GAP;
                if gapped > best.0 {
                    best = (gapped, best_prev.1);
                }
            }
            if best.0 > NEG {
                dp[j][i] = (best.0 + b, best.1);
            }
        }
    }

    let (mut score, mut pos) = (NEG, usize::MAX);
    for (i, cell) in dp[m - 1].iter().enumerate() {
        if cell.0 > score {
            score = cell.0;
            pos = i;
        }
    }
    if score <= NEG {
        return None;
    }
    let mut indices = vec![0; m];
    for j in (0..m).rev() {
        indices[j] = pos;
        pos = dp[j][pos].1;
    }
    // 路径越短越靠前
    Some(FuzzyMatch {
        score: score - n as i64 / 4,
        indices,
    })
}

/// 遍历得到的候选文件
pub struct Candidate {
    pub path: String,
    pub mtime: Option<SystemTime>,
}

/// 遍历工作区文件（遵循 .gitignore，包含隐藏文件但排除 `.git`，不跟随符号链接）。
/// `want_mtime` 为真时读取修改时间。返回 (候选, 是否因访问上限被截断)。
pub fn walk_workspace(workspace: &Path, want_mtime: bool) -> (Vec<Candidate>, bool) {
    let walker = WalkBuilder::new(workspace)
        .hidden(false)
        .follow_links(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    let mut out = Vec::new();
    let mut visited = 0usize;
    for entry in walker.flatten() {
        visited += 1;
        if visited > MAX_VISITED {
            return (out, true);
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(workspace) else {
            continue;
        };
        let mtime = if want_mtime {
            entry.metadata().ok().and_then(|m| m.modified().ok())
        } else {
            None
        };
        out.push(Candidate {
            path: rel.to_string_lossy().replace('\\', "/"),
            mtime,
        });
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATHS: [&str; 3] = [
        "src/provider/common.rs",
        "src/web/api.rs",
        "frontend/src/components/Parts.tsx",
    ];

    fn ranked(q: &str) -> Vec<&'static str> {
        let mut v: Vec<_> = PATHS
            .iter()
            .filter_map(|p| fuzzy_match(q, p).map(|m| (m.score, *p)))
            .collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.0));
        v.into_iter().map(|(_, p)| p).collect()
    }

    #[test]
    fn common_matches_only_common_rs() {
        assert_eq!(ranked("common"), vec!["src/provider/common.rs"]);
        let m = fuzzy_match("common", "src/provider/common.rs").unwrap();
        assert_eq!(m.indices, vec![13, 14, 15, 16, 17, 18]);
    }

    #[test]
    fn api_prefers_filename_hit() {
        assert_eq!(ranked("api")[0], "src/web/api.rs");
        let m = fuzzy_match("api", "src/web/api.rs").unwrap();
        assert_eq!(m.indices, vec![8, 9, 10]);
    }

    #[test]
    fn prts_matches_parts_tsx_with_camel_start() {
        assert_eq!(ranked("prts")[0], "frontend/src/components/Parts.tsx");
        let m = fuzzy_match("prts", "frontend/src/components/Parts.tsx").unwrap();
        assert_eq!(m.indices[0], 24);
    }

    #[test]
    fn wapi_spans_directory_and_name() {
        assert_eq!(ranked("wapi"), vec!["src/web/api.rs"]);
    }

    #[test]
    fn miss_and_case_insensitive() {
        assert!(fuzzy_match("zzz", "src/web/api.rs").is_none());
        assert!(fuzzy_match("PARTS", "frontend/src/components/Parts.tsx").is_some());
    }

    #[test]
    fn shorter_path_wins_on_tie() {
        let a = fuzzy_match("api", "api.rs").unwrap().score;
        let b = fuzzy_match("api", "some/deep/dir/api.rs").unwrap().score;
        assert!(a > b);
    }
}
