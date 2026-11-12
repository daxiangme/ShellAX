//! Navicat-style transfer for one database: dump selected tables to a SQL file, or run a SQL file.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{div, prelude::*, px, AnyElement, Context, SharedString, Window};
use ssh_term_proto::db::dialect;
use ssh_term_proto::i18n::{t, tf};

use crate::db_ui::{key_parts, DumpObject, SqlJob};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::Root;

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(Into::into)
        .unwrap_or_else(|| ".".into())
}

impl Root {
    pub(crate) fn db_open_dump(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(job) = self.transfer_job(key, true) else {
            return;
        };
        self.dbp.transfer = Some(job);
        self.load_dump_tables(cx);
        cx.notify();
    }

    pub(crate) fn db_open_import(&mut self, key: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(conn) = Self::db_conn(&key_parts(key).0) else {
            return;
        };
        if conn.read_only {
            self.toast(t("db.edit_read_only").into(), cx);
            return;
        }
        let Some(job) = self.transfer_job(key, false) else {
            return;
        };
        self.dbp.transfer = Some(job);
        cx.notify();
    }

    fn transfer_job(&self, key: &str, export: bool) -> Option<SqlJob> {
        let (conn, database, schema) = key_parts(key);
        let database = database?;
        if !self.db_up(&conn) {
            return None;
        }
        Some(SqlJob {
            conn,
            database,
            schema,
            export,
            tables: Vec::new(),
            loading: export,
            structure: true,
            data: true,
            drop: false,
            path: String::new(),
            running: false,
            progress: String::new(),
            error: None,
            done: None,
            slot: None,
        })
    }

    fn load_dump_tables(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.dbp.transfer.as_ref() else {
            return;
        };
        let (Some(live), Some(conn)) = (self.db_ready(&job.conn), Self::db_conn(&job.conn)) else {
            return;
        };
        let (id, db, schema) = (job.conn.clone(), job.database.clone(), job.schema.clone());
        let sql = dialect::list_tables(conn.kind.family(), &db, schema.as_deref());
        let h = self.rt.spawn(async move { live.driver.execute(&db, &sql, true).await });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                let Some(job) = this.dbp.transfer.as_mut() else {
                    return;
                };
                if job.conn != id {
                    return;
                }
                match r {
                    Ok(sets) => {
                        if let Some(e) = sets.iter().find_map(|s| s.error.clone()) {
                            job.error = Some(e);
                            job.loading = false;
                        } else {
                            let mut tables = Vec::new();
                            if let Some(set) = sets.first() {
                                for row in &set.rows {
                                    let name = row.first().cloned().flatten().unwrap_or_default();
                                    if name.is_empty() {
                                        continue;
                                    }
                                    let kind = row.get(1).cloned().flatten().unwrap_or_default();
                                    let view = kind.to_ascii_uppercase().contains("VIEW");
                                    tables.push(DumpObject {
                                        name,
                                        view,
                                        on: !view,
                                    });
                                }
                            }
                            job.tables = tables;
                            job.loading = false;
                        }
                    }
                    Err(e) => {
                        job.error = Some(format!("{e:#}"));
                        job.loading = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn db_transfer_stop(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.dbp.transfer.as_ref() else {
            return;
        };
        let Some(slot) = job.slot.clone() else {
            return;
        };
        let (conn, db) = (job.conn.clone(), job.database.clone());
        if let Some(job) = self.dbp.transfer.as_mut() {
            job.progress = t("db.cancelling").into();
        }
        if let Some(live) = self.db_ready(&conn) {
            self.rt.spawn(async move {
                let _ = live.driver.cancel(&db, &slot).await;
            });
        }
        cx.notify();
    }

    fn db_transfer_close(&mut self, cx: &mut Context<Self>) {
        let running = self
            .dbp
            .transfer
            .as_ref()
            .is_some_and(|j| j.running);
        if running {
            self.db_transfer_stop(cx);
        }
        self.dbp.transfer = None;
        cx.notify();
    }

    pub(crate) fn db_transfer_dialog(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(job) = self.dbp.transfer.as_ref() else {
            return div().into_any_element();
        };
        let th = cur();
        let export = job.export;
        let loading = job.loading;
        let running = job.running;
        let done = job.done.clone();
        let progress = job.progress.clone();
        let error = job.error.clone();
        let path = job.path.clone();
        let database = job.database.clone();
        let structure = job.structure;
        let data = job.data;
        let drop = job.drop;
        let tables = job.tables.clone();
        let title = if export {
            t("dbt.dump_sql")
        } else {
            t("dbt.run_sql")
        };
        let mut body = div()
            .p(px(ui::SP_5))
            .flex()
            .flex_col()
            .gap(px(ui::SP_3))
            .max_h(px(520.));
        if export {
            body = body
                .child(
                    div()
                        .text_size(px(ui::fs_small()))
                        .text_color(c(th.subtext))
                        .child(format!("{} · {database}", t("dbt.dump_tables"))),
                )
                .child(
                    div().flex().gap(px(ui::SP_2)).child(
                        ui::chip("dump-all", t("dbt.dump_all"), false).on_click(cx.listener(
                            |this, _, _, cx| {
                                if let Some(job) = this.dbp.transfer.as_mut() {
                                    if !job.running {
                                        for table in &mut job.tables {
                                            table.on = true;
                                        }
                                    }
                                }
                                cx.notify();
                            },
                        )),
                    )
                    .child(ui::chip("dump-none", t("dbt.dump_none"), false).on_click(
                        cx.listener(|this, _, _, cx| {
                            if let Some(job) = this.dbp.transfer.as_mut() {
                                if !job.running {
                                    for table in &mut job.tables {
                                        table.on = false;
                                    }
                                }
                            }
                            cx.notify();
                        }),
                    ))
                    .child(ui::chip("dump-structure", t("dbt.dump_structure"), structure).on_click(
                        cx.listener(|this, _, _, cx| {
                            if let Some(job) = this.dbp.transfer.as_mut() {
                                if !job.running {
                                    job.structure = !job.structure;
                                }
                            }
                            cx.notify();
                        }),
                    ))
                    .child(
                        ui::chip("dump-data", t("dbt.dump_data"), data).on_click(cx.listener(
                            |this, _, _, cx| {
                                if let Some(job) = this.dbp.transfer.as_mut() {
                                    if !job.running {
                                        job.data = !job.data;
                                    }
                                }
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        ui::chip("dump-drop", t("dbt.dump_drop"), drop).on_click(cx.listener(
                            |this, _, _, cx| {
                                if let Some(job) = this.dbp.transfer.as_mut() {
                                    if !job.running {
                                        job.drop = !job.drop;
                                    }
                                }
                                cx.notify();
                            },
                        )),
                    ),
                );
            if loading {
                body = body.child(
                    div()
                        .text_size(px(ui::fs_body()))
                        .text_color(c(th.subtext))
                        .child(t("db.loading")),
                );
            } else {
                let mut list = div()
                    .id("dump-tables")
                    .w_full()
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .p_1()
                    .rounded(px(ui::R_MD))
                    .bg(crate::theme::sub_bg())
                    .border_1()
                    .border_color(crate::theme::divider());
                if tables.is_empty() {
                    list = list.child(
                        div()
                            .px_2()
                            .py_2()
                            .text_size(px(ui::fs_small()))
                            .text_color(c(th.overlay1))
                            .child(t("db.no_objects")),
                    );
                }
                for (i, table) in tables.iter().enumerate() {
                    let on = table.on;
                    let name = table.name.clone();
                    list = list.child(
                        div()
                            .id(("dump-obj", i))
                            .px_2()
                            .py(px(4.))
                            .rounded(px(ui::R_SM))
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .hover(|d| d.bg(crate::theme::hover_bg()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(job) = this.dbp.transfer.as_mut() {
                                    if job.running {
                                        return;
                                    }
                                    if let Some(table) = job.tables.get_mut(i) {
                                        table.on = !table.on;
                                    }
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .size(px(14.))
                                    .flex_shrink_0()
                                    .rounded(px(3.))
                                    .border_1()
                                    .border_color(if on { c(th.accent) } else { c(th.surface1) })
                                    .bg(if on { c(th.accent) } else { c(th.base) })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(on, |d| {
                                        d.child(icon(IconName::Check, ui::IC_XS, c(th.base)))
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .font_family(ui::mono_family())
                                    .text_size(px(ui::fs_body()))
                                    .text_color(c(th.text))
                                    .child(name),
                            )
                            .when(table.view, |d| {
                                d.child(
                                    div()
                                        .text_size(px(ui::fs_caption()))
                                        .text_color(c(th.overlay1))
                                        .child("VIEW"),
                                )
                            }),
                    );
                }
                body = body.child(list);
            }
        } else {
            body = body
                .child(
                    div()
                        .text_size(px(ui::fs_body()))
                        .text_color(c(th.text))
                        .child(tf("dbt.run_warn", &[("db", &database)])),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(ui::fs_small()))
                                .text_color(c(if path.is_empty() { th.overlay1 } else { th.text }))
                                .child(if path.is_empty() {
                                    t("dbt.run_pick").to_string()
                                } else {
                                    path.clone()
                                }),
                        )
                        .child(
                            ui::button("sql-pick", t("dbt.run_pick"), 0).on_click(cx.listener(
                                |this, _, _w, cx| this.db_pick_sql(cx),
                            )),
                        ),
                );
        }
        if !progress.is_empty() {
            body = body.child(
                div()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.accent))
                    .child(progress),
            );
        }
        if let Some(done) = done.clone() {
            body = body.child(
                div()
                    .text_size(px(ui::fs_body()))
                    .text_color(c(th.success))
                    .child(done),
            );
        }
        if let Some(error) = error.clone() {
            body = body.child(
                div()
                    .text_size(px(ui::fs_body()))
                    .text_color(c(th.error))
                    .child(error),
            );
        }
        let me_close = cx.entity().downgrade();
        let close = ui::dialog_close("sqljob-x", move |_w, cx| {
            let _ = me_close.update(cx, |this, cx| this.db_transfer_close(cx));
        });
        let mut buttons: Vec<AnyElement> = vec![ui::button("sqljob-cancel", t("ui.cancel"), 0)
            .on_click(cx.listener(|this, _, _, cx| this.db_transfer_close(cx)))
            .into_any_element()];
        if running {
            buttons.push(
                ui::button("sqljob-stop", t("db.stop"), 2)
                    .on_click(cx.listener(|this, _, _, cx| this.db_transfer_stop(cx)))
                    .into_any_element(),
            );
        } else if done.is_none() {
            buttons.push(
                ui::button(
                    "sqljob-go",
                    if export { t("dbt.dump_start") } else { t("dbt.run_sql") },
                    1,
                )
                .when(loading, |b| b.opacity(0.5))
                .on_click(cx.listener(move |this, _, _w, cx| {
                    if export {
                        this.db_start_dump(cx);
                    } else {
                        this.db_start_import(cx);
                    }
                }))
                .into_any_element(),
            );
        }
        ui::modal(
            ui::card(560.)
                .child(ui::dialog_header(
                    Some(if export {
                        IconName::Download
                    } else {
                        IconName::Import
                    }),
                    c(th.accent),
                    title,
                    Some(SharedString::from(database)),
                    Some(close),
                ))
                .child(body)
                .child(ui::dialog_footer(None, buttons)),
        )
        .into_any_element()
    }

    fn db_pick_sql(&mut self, cx: &mut Context<Self>) {
        if self.dbp.transfer.as_ref().is_some_and(|j| j.running) {
            return;
        }
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(job) = this.dbp.transfer.as_mut() {
                    job.path = path.display().to_string();
                    job.error = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn db_start_dump(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.dbp.transfer.as_ref() else {
            return;
        };
        if job.loading || job.running {
            return;
        }
        if !job.structure && !job.data {
            self.dbp.transfer.as_mut().unwrap().error = Some(t("dbt.dump_need").into());
            cx.notify();
            return;
        }
        let tables: Vec<(String, bool)> = job
            .tables
            .iter()
            .filter(|t| t.on)
            .map(|t| (t.name.clone(), t.view))
            .collect();
        if tables.is_empty() {
            self.dbp.transfer.as_mut().unwrap().error = Some(t("dbt.dump_no_table").into());
            cx.notify();
            return;
        }
        let (structure, data, drop, db_name) = (job.structure, job.data, job.drop, job.database.clone());
        let suggest = format!("{db_name}.sql");
        let rx = cx.prompt_for_new_path(&home_dir(), Some(&suggest));
        let (conn, database, schema) = (job.conn.clone(), job.database.clone(), job.schema.clone());
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return;
            };
            let slot = Arc::new(ssh_term_proto::db::CancelSlot::default());
            let progress = Arc::new(Mutex::new(String::new()));
            let ready = this.update(cx, |this, cx| {
                let Some(live) = this.db_ready(&conn) else {
                    if let Some(job) = this.dbp.transfer.as_mut() {
                        job.error = Some(t("db.not_connected").into());
                    }
                    cx.notify();
                    return None;
                };
                if let Some(job) = this.dbp.transfer.as_mut() {
                    job.running = true;
                    job.error = None;
                    job.done = None;
                    job.path = path.display().to_string();
                    job.slot = Some(slot.clone());
                    job.progress = t("db.exporting").into();
                }
                cx.notify();
                Some(live)
            });
            let Ok(Some(live)) = ready else {
                return;
            };
            let prog = progress.clone();
            let slot2 = slot.clone();
            let path2 = path.clone();
            let h = this
                .update(cx, |this, _cx| {
                    this.rt.spawn(async move {
                        live.driver
                            .dump_sql(
                                &database,
                                schema.as_deref(),
                                &tables,
                                structure,
                                data,
                                drop,
                                &path2,
                                Some(slot2),
                                &prog,
                            )
                            .await
                    })
                })
                .ok();
            let Some(h) = h else {
                return;
            };
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let text = progress.lock().ok().map(|g| g.clone()).unwrap_or_default();
                let finished = h.is_finished();
                let stop = this
                    .update(cx, |this, cx| {
                        let Some(job) = this.dbp.transfer.as_mut() else {
                            return true;
                        };
                        if !text.is_empty() {
                            job.progress = text;
                        }
                        cx.notify();
                        finished || !job.running
                    })
                    .unwrap_or(true);
                if stop {
                    break;
                }
            }
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                let Some(job) = this.dbp.transfer.as_mut() else {
                    return;
                };
                job.running = false;
                job.slot = None;
                match r {
                    Ok((n, rows)) => {
                        job.done = Some(tf(
                            "dbt.dump_done",
                            &[
                                ("tables", &n),
                                ("rows", &rows),
                                ("path", &path.display()),
                            ],
                        ));
                        job.progress.clear();
                    }
                    Err(e) => job.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn db_start_import(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.dbp.transfer.as_ref() else {
            return;
        };
        if job.running {
            return;
        }
        if job.path.trim().is_empty() {
            self.dbp.transfer.as_mut().unwrap().error = Some(t("dbt.run_pick").into());
            cx.notify();
            return;
        }
        let path = std::path::PathBuf::from(&job.path);
        let (conn, database, schema) = (job.conn.clone(), job.database.clone(), job.schema.clone());
        let slot = Arc::new(ssh_term_proto::db::CancelSlot::default());
        let progress = Arc::new(Mutex::new(String::new()));
        let Some(live) = self.db_ready(&conn) else {
            self.dbp.transfer.as_mut().unwrap().error = Some(t("db.not_connected").into());
            cx.notify();
            return;
        };
        if let Some(job) = self.dbp.transfer.as_mut() {
            job.running = true;
            job.error = None;
            job.done = None;
            job.slot = Some(slot.clone());
            job.progress = t("dbt.run_progress").replace("{i}", "0").replace("{n}", "…");
        }
        cx.notify();
        let prog = progress.clone();
        let h = self.rt.spawn(async move {
            let text = std::fs::read_to_string(&path)?;
            let text = text.trim_start_matches('\u{feff}');
            live.driver
                .run_sql_script(&database, schema.as_deref(), text, Some(slot), &prog)
                .await
        });
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let text = progress.lock().ok().map(|g| g.clone()).unwrap_or_default();
                let finished = h.is_finished();
                let stop = this
                    .update(cx, |this, cx| {
                        let Some(job) = this.dbp.transfer.as_mut() else {
                            return true;
                        };
                        if !text.is_empty() {
                            job.progress = text;
                        }
                        cx.notify();
                        finished || !job.running
                    })
                    .unwrap_or(true);
                if stop {
                    break;
                }
            }
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                let Some(job) = this.dbp.transfer.as_mut() else {
                    return;
                };
                job.running = false;
                job.slot = None;
                match r {
                    Ok(n) => {
                        job.done = Some(tf("dbt.run_done", &[("n", &n)]));
                        job.progress.clear();
                    }
                    Err(e) => job.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
