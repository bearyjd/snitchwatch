//! Bound the library iterator's work, including paths that yield no records.

const UNVISITED: u8 = 254;
const VISITING: u8 = 255;

pub(super) fn validate(
    bytes: &[u8],
    metadata: &maxminddb::Metadata,
    max_steps: usize,
) -> Result<(), String> {
    let node_count = metadata.node_count as usize;
    let node_bytes = match metadata.record_size {
        24 => 6,
        28 => 7,
        32 => 8,
        _ => return Err("The selected database has an unsupported tree record size.".into()),
    };
    let tree_len = node_count
        .checked_mul(node_bytes)
        .filter(|&len| len <= bytes.len())
        .ok_or("The selected database has a truncated search tree.")?;
    let mut tree = Tree {
        bytes: &bytes[..tree_len],
        node_bytes,
        node_count,
        max_depth: if metadata.ip_version == 4 { 32 } else { 128 },
        max_steps,
        // Five bytes per declared node, bounded by the input file size.
        heights: vec![UNVISITED; node_count],
        steps: vec![0; node_count],
    };
    tree.visit(0, 0)?;
    Ok(())
}

struct Tree<'a> {
    bytes: &'a [u8],
    node_bytes: usize,
    node_count: usize,
    max_depth: u8,
    max_steps: usize,
    heights: Vec<u8>,
    steps: Vec<u32>,
}

impl Tree<'_> {
    /// Memoize each subtree's height and expanded traversal cost. A shared
    /// subtree can be a valid alias, but a tiny DAG can also expand into
    /// billions of paths. Counting only unique nodes would miss that case.
    fn visit(&mut self, node: usize, depth: u8) -> Result<(u32, u8), String> {
        if depth > self.max_depth {
            return Err("The selected database search tree exceeds its IP address depth.".into());
        }
        if node >= self.node_count {
            if node > self.node_count && node < self.node_count + 16 {
                return Err("The selected database has an invalid search tree pointer.".into());
            }
            return Ok((1, 0));
        }
        match self.heights[node] {
            VISITING => return Err("The selected database search tree contains a cycle.".into()),
            UNVISITED => {}
            height => {
                if depth as u16 + height as u16 > self.max_depth as u16 {
                    return Err(
                        "The selected database search tree exceeds its IP address depth.".into(),
                    );
                }
                return Ok((self.steps[node], height));
            }
        }
        // Only recurse within an IP prefix, so the call stack is at most 129.
        if depth == self.max_depth {
            return Err("The selected database search tree exceeds its IP address depth.".into());
        }
        self.heights[node] = VISITING;
        let (left_steps, left_height) = self.visit(self.child(node, 0), depth + 1)?;
        let (right_steps, right_height) = self.visit(self.child(node, 1), depth + 1)?;
        let steps = 1u64 + left_steps as u64 + right_steps as u64;
        if steps > self.max_steps as u64 {
            return Err(
                "The selected database search tree contains too many network paths.".into(),
            );
        }
        let height = 1 + left_height.max(right_height);
        self.heights[node] = height;
        self.steps[node] = steps as u32;
        Ok((steps as u32, height))
    }

    // MMDB's fixed search-tree encoding. Data records remain decoded and
    // validated by maxminddb; no second data-format decoder is needed.
    fn child(&self, node: usize, side: usize) -> usize {
        let base = node * self.node_bytes;
        let offset = base + side * (self.node_bytes / 2 + self.node_bytes % 2);
        match self.node_bytes {
            6 => self.u24(offset),
            7 => {
                let middle = self.bytes[base + 3];
                let high = if side == 0 {
                    middle >> 4
                } else {
                    middle & 0x0f
                };
                (high as usize) << 24 | self.u24(offset)
            }
            8 => (self.bytes[offset] as usize) << 24 | self.u24(offset + 1),
            _ => unreachable!(),
        }
    }

    fn u24(&self, offset: usize) -> usize {
        (self.bytes[offset] as usize) << 16
            | (self.bytes[offset + 1] as usize) << 8
            | self.bytes[offset + 2] as usize
    }
}
