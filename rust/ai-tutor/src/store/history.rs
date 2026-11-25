//! Read-only, owner-scoped conversation pages. This is a UI history projection,
//! never a replacement for the live teaching-context window.
use super::*;

impl Store {
    pub fn turn_history_page(
        &self,
        lid: &str,
        sid: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Value> {
        ensure!((1..=50).contains(&limit), "历史页大小必须为1至50");
        ensure!(before.is_none_or(|cursor| cursor > 0), "历史游标无效");
        ensure!(self.one(
            "SELECT s.session_id FROM sessions s JOIN learners l ON l.learner_id=s.learner_id WHERE s.session_id=? AND s.learner_id=?",
            params![sid, lid],
        )?.is_some(), "活动不存在或不属于当前档案");
        let projection = "SELECT t.rowid AS history_cursor,t.turn_id,t.role,t.content,t.status,t.created_at,CASE WHEN d.request_id IS NOT NULL THEN 'segments' END AS delivery_mode,d.phase AS stream_phase FROM turns t JOIN sessions s ON s.session_id=t.session_id AND s.learner_id=t.learner_id LEFT JOIN tutor_streams d ON d.turn_id=t.turn_id WHERE t.session_id=? AND t.learner_id=?";
        let mut rows = match before {
            Some(cursor) => self.rows(
                &format!("{projection} AND t.rowid<? ORDER BY t.rowid DESC LIMIT ?"),
                params![sid, lid, cursor, (limit + 1) as i64],
            )?,
            None => self.rows(
                &format!("{projection} ORDER BY t.rowid DESC LIMIT ?"),
                params![sid, lid, (limit + 1) as i64],
            )?,
        };
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next_before = more
            .then(|| rows.last().and_then(|row| row["history_cursor"].as_i64()))
            .flatten();
        for row in &mut rows {
            row.as_object_mut().unwrap().remove("history_cursor");
            if row["delivery_mode"].is_null() {
                row.as_object_mut().unwrap().remove("delivery_mode");
                row.as_object_mut().unwrap().remove("stream_phase");
            }
        }
        rows.reverse();
        Ok(json!({"learner_id":lid,"session_id":sid,"items":rows,"next_before":next_before}))
    }
}
