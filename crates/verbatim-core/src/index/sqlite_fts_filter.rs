fn fts_counts_empty_and_aligned(counts: FtsMaintenanceCounts) -> bool {
    counts.child_rows == 0
        && counts.fts_rows == 0
        && counts.missing_rows == 0
        && counts.orphan_rows == 0
}

fn search_filtered_fts(
    store: &Store,
    query: &str,
    top_k: usize,
    source_filter: &HashSet<SourceId>,
) -> Result<Vec<(ChunkId, f32)>> {
    if top_k == 0 || source_filter.is_empty() {
        return Ok(Vec::new());
    }
    let Some(fts_query) = normalize_fts_query(query) else {
        return Ok(Vec::new());
    };

    let mut source_ids = source_filter
        .iter()
        .map(|source_id| source_id.0.clone())
        .collect::<Vec<_>>();
    source_ids.sort_unstable();
    let source_placeholders = (2..source_ids.len() + 2)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let limit_placeholder = source_ids.len() + 2;
    let mut stmt = store
        .connection()
        .prepare(&format!(
            "SELECT chunk_id, bm25(chunk_fts) AS rank
             FROM chunk_fts
             WHERE chunk_fts MATCH ?1
               AND source_id IN ({source_placeholders})
             ORDER BY rank
             LIMIT ?{limit_placeholder}"
        ))
        .context("prepare filtered FTS search")?;
    let mut query_params = Vec::with_capacity(source_ids.len() + 2);
    query_params.push(Value::Text(fts_query));
    query_params.extend(source_ids.into_iter().map(Value::Text));
    query_params.push(Value::Integer(top_k as i64));
    let rows = stmt
        .query_map(params_from_iter(query_params.iter()), |row| {
            let chunk_id: String = row.get(0)?;
            let rank: f64 = row.get(1)?;
            let score = 1.0 / (1.0 + rank.abs() as f32);
            Ok((ChunkId(chunk_id), score))
        })
        .context("execute filtered FTS search")?;

    rows.map(|row| row.map_err(Into::into)).collect()
}
