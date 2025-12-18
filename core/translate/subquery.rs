use std::sync::Arc;

use turso_parser::ast::{self, SortOrder, SubqueryType};

use crate::{
    emit_explain,
    schema::{Index, IndexColumn, RecursiveCte, Table},
    translate::{
        collate::get_collseq_from_expr,
        emitter::emit_program_for_select,
        expr::{unwrap_parens, walk_expr_mut, WalkControl},
        optimizer::optimize_select_plan,
        plan::{
            ColumnUsedMask, NonFromClauseSubquery, OuterQueryReference, Plan, SubqueryPosition,
            SubqueryState,
        },
        select::prepare_select_plan,
    },
    vdbe::{
        builder::{CursorKey, CursorType, ProgramBuilder},
        insn::Insn,
        BranchOffset, CursorID,
    },
    Connection, QueryMode, Result,
};

use super::{
    emitter::{emit_query, Resolver, TranslateCtx},
    main_loop::LoopLabels,
    plan::{JoinedTable, Operation, QueryDestination, Scan, Search, SelectPlan, TableReferences},
};

/// Maximum recursion depth for recursive CTEs to prevent infinite loops.
/// This matches SQLite's default behavior.
const RECURSIVE_CTE_MAX_ITERATIONS: i64 = 1000;

// Compute query plans for subqueries occurring in any position other than the FROM clause.
// This includes the WHERE clause, HAVING clause, GROUP BY clause, ORDER BY clause, LIMIT clause, and OFFSET clause.
/// The AST expression containing the subquery ([ast::Expr::Exists], [ast::Expr::Subquery], [ast::Expr::InSelect]) is replaced with a [ast::Expr::SubqueryResult] expression.
/// The [ast::Expr::SubqueryResult] expression contains the subquery ID, the left-hand side expression (only applicable to IN subqueries), the NOT IN flag (only applicable to IN subqueries), and the subquery type.
/// The computed plans are stored in the [NonFromClauseSubquery] structs on the [SelectPlan], and evaluated at the appropriate time during the translation of the main query.
/// The appropriate time is determined by whether the subquery is correlated or uncorrelated;
/// if it is uncorrelated, it can be evaluated as early as possible, but if it is correlated, it must be evaluated after all of its dependencies from the
/// outer query are 'in scope', i.e. their cursors are open and rewound.
pub fn plan_subqueries_from_select_plan(
    program: &mut ProgramBuilder,
    plan: &mut SelectPlan,
    resolver: &Resolver,
    connection: &Arc<Connection>,
) -> Result<()> {
    // WHERE
    plan_subqueries_with_outer_query_access(
        program,
        &mut plan.non_from_clause_subqueries,
        &mut plan.table_references,
        resolver,
        plan.where_clause.iter_mut().map(|t| &mut t.expr),
        connection,
        SubqueryPosition::Where,
    )?;

    // GROUP BY
    if let Some(group_by) = &mut plan.group_by {
        plan_subqueries_with_outer_query_access(
            program,
            &mut plan.non_from_clause_subqueries,
            &mut plan.table_references,
            resolver,
            group_by.exprs.iter_mut(),
            connection,
            SubqueryPosition::GroupBy,
        )?;
        if let Some(having) = group_by.having.as_mut() {
            plan_subqueries_with_outer_query_access(
                program,
                &mut plan.non_from_clause_subqueries,
                &mut plan.table_references,
                resolver,
                having.iter_mut(),
                connection,
                SubqueryPosition::Having,
            )?;
        }
    }

    // Result columns
    plan_subqueries_with_outer_query_access(
        program,
        &mut plan.non_from_clause_subqueries,
        &mut plan.table_references,
        resolver,
        plan.result_columns.iter_mut().map(|c| &mut c.expr),
        connection,
        SubqueryPosition::ResultColumn,
    )?;

    // ORDER BY
    plan_subqueries_with_outer_query_access(
        program,
        &mut plan.non_from_clause_subqueries,
        &mut plan.table_references,
        resolver,
        plan.order_by.iter_mut().map(|(expr, _)| &mut **expr),
        connection,
        SubqueryPosition::OrderBy,
    )?;

    // LIMIT and OFFSET cannot reference columns from the outer query
    let get_outer_query_refs = |_: &TableReferences| vec![];
    {
        let mut subquery_parser = get_subquery_parser(
            program,
            &mut plan.non_from_clause_subqueries,
            &mut plan.table_references,
            resolver,
            connection,
            get_outer_query_refs,
            SubqueryPosition::LimitOffset,
        );
        // Limit
        if let Some(limit) = &mut plan.limit {
            walk_expr_mut(limit, &mut subquery_parser)?;
        }
        // Offset
        if let Some(offset) = &mut plan.offset {
            walk_expr_mut(offset, &mut subquery_parser)?;
        }
    }

    update_column_used_masks(
        &mut plan.table_references,
        &mut plan.non_from_clause_subqueries,
    );
    Ok(())
}

/// Compute query plans for subqueries in the WHERE clause and HAVING clause (both of which have access to the outer query scope)
fn plan_subqueries_with_outer_query_access<'a>(
    program: &mut ProgramBuilder,
    out_subqueries: &mut Vec<NonFromClauseSubquery>,
    referenced_tables: &mut TableReferences,
    resolver: &Resolver,
    exprs: impl Iterator<Item = &'a mut ast::Expr>,
    connection: &Arc<Connection>,
    position: SubqueryPosition,
) -> Result<()> {
    // Most subqueries can reference columns from the outer query,
    // including nested cases where a subquery inside a subquery references columns from its parent's parent
    // and so on.
    let get_outer_query_refs = |referenced_tables: &TableReferences| {
        referenced_tables
            .joined_tables()
            .iter()
            .map(|t| OuterQueryReference {
                table: t.table.clone(),
                identifier: t.identifier.clone(),
                internal_id: t.internal_id,
                col_used_mask: ColumnUsedMask::default(),
            })
            .chain(
                referenced_tables
                    .outer_query_refs()
                    .iter()
                    .map(|t| OuterQueryReference {
                        table: t.table.clone(),
                        identifier: t.identifier.clone(),
                        internal_id: t.internal_id,
                        col_used_mask: ColumnUsedMask::default(),
                    }),
            )
            .collect::<Vec<_>>()
    };

    let mut subquery_parser = get_subquery_parser(
        program,
        out_subqueries,
        referenced_tables,
        resolver,
        connection,
        get_outer_query_refs,
        position,
    );
    for expr in exprs {
        walk_expr_mut(expr, &mut subquery_parser)?;
    }

    Ok(())
}

/// Create a closure that will walk the AST and replace subqueries with [ast::Expr::SubqueryResult] expressions.
fn get_subquery_parser<'a>(
    program: &'a mut ProgramBuilder,
    out_subqueries: &'a mut Vec<NonFromClauseSubquery>,
    referenced_tables: &'a mut TableReferences,
    resolver: &'a Resolver,
    connection: &'a Arc<Connection>,
    get_outer_query_refs: fn(&TableReferences) -> Vec<OuterQueryReference>,
    position: SubqueryPosition,
) -> impl FnMut(&mut ast::Expr) -> Result<WalkControl> + 'a {
    fn handle_unsupported_correlation(correlated: bool, position: SubqueryPosition) -> Result<()> {
        if correlated && !position.allow_correlated() {
            crate::bail_parse_error!(
                "correlated subqueries in {} clause are not supported yet",
                position.name()
            );
        }
        Ok(())
    }

    move |expr: &mut ast::Expr| -> Result<WalkControl> {
        match expr {
            ast::Expr::Exists(_) => {
                let subquery_id = program.table_reference_counter.next();
                let outer_query_refs = get_outer_query_refs(referenced_tables);

                let result_reg = program.alloc_register();
                let subquery_type = SubqueryType::Exists { result_reg };
                let result_expr = ast::Expr::SubqueryResult {
                    subquery_id,
                    lhs: None,
                    not_in: false,
                    query_type: subquery_type.clone(),
                };
                let ast::Expr::Exists(subselect) = std::mem::replace(expr, result_expr) else {
                    unreachable!();
                };

                let plan = prepare_select_plan(
                    subselect,
                    resolver,
                    program,
                    &outer_query_refs,
                    QueryDestination::ExistsSubqueryResult { result_reg },
                    connection,
                )?;
                let Plan::Select(mut plan) = plan else {
                    crate::bail_parse_error!(
                        "compound SELECT queries not supported yet in WHERE clause subqueries"
                    );
                };
                optimize_select_plan(&mut plan, resolver.schema)?;
                // EXISTS subqueries are satisfied after at most 1 row has been returned.
                plan.limit = Some(Box::new(ast::Expr::Literal(ast::Literal::Numeric(
                    "1".to_string(),
                ))));
                let correlated = plan.is_correlated();
                handle_unsupported_correlation(correlated, position)?;
                out_subqueries.push(NonFromClauseSubquery {
                    internal_id: subquery_id,
                    query_type: subquery_type,
                    state: SubqueryState::Unevaluated {
                        plan: Some(Box::new(plan)),
                    },
                    correlated,
                });
                Ok(WalkControl::Continue)
            }
            ast::Expr::Subquery(_) => {
                let subquery_id = program.table_reference_counter.next();
                let outer_query_refs = get_outer_query_refs(referenced_tables);

                let result_expr = ast::Expr::SubqueryResult {
                    subquery_id,
                    lhs: None,
                    not_in: false,
                    // Placeholder values because the number of columns returned is not known until the plan is prepared.
                    // These are replaced below after planning.
                    query_type: SubqueryType::RowValue {
                        result_reg_start: 0,
                        num_regs: 0,
                    },
                };
                let ast::Expr::Subquery(subselect) = std::mem::replace(expr, result_expr) else {
                    unreachable!();
                };
                let plan = prepare_select_plan(
                    subselect,
                    resolver,
                    program,
                    &outer_query_refs,
                    QueryDestination::Unset,
                    connection,
                )?;
                let Plan::Select(mut plan) = plan else {
                    crate::bail_parse_error!(
                        "compound SELECT queries not supported yet in WHERE clause subqueries"
                    );
                };
                optimize_select_plan(&mut plan, resolver.schema)?;
                let reg_count = plan.result_columns.len();
                let reg_start = program.alloc_registers(reg_count);

                plan.query_destination = QueryDestination::RowValueSubqueryResult {
                    result_reg_start: reg_start,
                    num_regs: reg_count,
                };
                // RowValue subqueries are satisfied after at most 1 row has been returned,
                // as they are used in comparisons with a scalar or a tuple of scalars like (x,y) = (SELECT ...) or x = (SELECT ...).
                plan.limit = Some(Box::new(ast::Expr::Literal(ast::Literal::Numeric(
                    "1".to_string(),
                ))));

                let ast::Expr::SubqueryResult {
                    subquery_id,
                    lhs: None,
                    not_in: false,
                    query_type:
                        SubqueryType::RowValue {
                            result_reg_start,
                            num_regs,
                        },
                } = &mut *expr
                else {
                    unreachable!();
                };
                *result_reg_start = reg_start;
                *num_regs = reg_count;

                let correlated = plan.is_correlated();
                handle_unsupported_correlation(correlated, position)?;

                out_subqueries.push(NonFromClauseSubquery {
                    internal_id: *subquery_id,
                    query_type: SubqueryType::RowValue {
                        result_reg_start: reg_start,
                        num_regs: reg_count,
                    },
                    state: SubqueryState::Unevaluated {
                        plan: Some(Box::new(plan)),
                    },
                    correlated,
                });
                Ok(WalkControl::Continue)
            }
            ast::Expr::InSelect { .. } => {
                let subquery_id = program.table_reference_counter.next();
                let outer_query_refs = get_outer_query_refs(referenced_tables);

                let ast::Expr::InSelect { lhs, not, rhs } =
                    std::mem::replace(expr, ast::Expr::Literal(ast::Literal::Null))
                else {
                    unreachable!();
                };
                let plan = prepare_select_plan(
                    rhs,
                    resolver,
                    program,
                    &outer_query_refs,
                    QueryDestination::Unset,
                    connection,
                )?;
                let Plan::Select(mut plan) = plan else {
                    crate::bail_parse_error!(
                        "compound SELECT queries not supported yet in WHERE clause subqueries"
                    );
                };
                optimize_select_plan(&mut plan, resolver.schema)?;
                // e.g. (x,y) IN (SELECT ...)
                // or x IN (SELECT ...)
                let lhs_column_count = match unwrap_parens(lhs.as_ref())? {
                    ast::Expr::Parenthesized(exprs) => exprs.len(),
                    _ => 1,
                };
                if lhs_column_count != plan.result_columns.len() {
                    crate::bail_parse_error!(
                        "lhs of IN subquery must have the same number of columns as the subquery"
                    );
                }

                let mut columns = plan
                    .result_columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| IndexColumn {
                        name: c.name(&plan.table_references).unwrap_or("").to_string(),
                        order: SortOrder::Asc,
                        pos_in_table: i,
                        collation: None,
                        default: None,
                        expr: None,
                    })
                    .collect::<Vec<_>>();

                for (i, column) in columns.iter_mut().enumerate() {
                    column.collation = get_collseq_from_expr(
                        &plan.result_columns[i].expr,
                        &plan.table_references,
                    )?;
                }

                let ephemeral_index = Arc::new(Index {
                    columns,
                    name: format!("ephemeral_index_where_sub_{subquery_id}"),
                    table_name: String::new(),
                    ephemeral: true,
                    has_rowid: false,
                    root_page: 0,
                    unique: false,
                    where_clause: None,
                    index_method: None,
                });

                let cursor_id =
                    program.alloc_cursor_id(CursorType::BTreeIndex(ephemeral_index.clone()));

                plan.query_destination = QueryDestination::EphemeralIndex {
                    cursor_id,
                    index: ephemeral_index.clone(),
                    is_delete: false,
                };

                *expr = ast::Expr::SubqueryResult {
                    subquery_id,
                    lhs: Some(lhs),
                    not_in: not,
                    query_type: SubqueryType::In { cursor_id },
                };

                let correlated = plan.is_correlated();
                handle_unsupported_correlation(correlated, position)?;

                out_subqueries.push(NonFromClauseSubquery {
                    internal_id: subquery_id,
                    query_type: SubqueryType::In { cursor_id },
                    state: SubqueryState::Unevaluated {
                        plan: Some(Box::new(plan)),
                    },
                    correlated,
                });
                Ok(WalkControl::Continue)
            }
            _ => Ok(WalkControl::Continue),
        }
    }
}

/// We make decisions about when to evaluate expressions or whether to use covering indexes based on
/// which columns of a table have been referenced.
/// Since subquery nesting is arbitrarily deep, a reference to a column must propagate recursively
/// up to the parent. Example:
///
/// SELECT * FROM t WHERE EXISTS (SELECT * FROM u WHERE EXISTS (SELECT * FROM v WHERE v.foo = t.foo))
///
/// In this case, t.foo is referenced in the innermost subquery, so the top level query must be notified
/// that t.foo has been used.
fn update_column_used_masks(
    table_refs: &mut TableReferences,
    subqueries: &mut [NonFromClauseSubquery],
) {
    for subquery in subqueries.iter_mut() {
        let SubqueryState::Unevaluated { plan } = &mut subquery.state else {
            panic!("subquery has already been evaluated");
        };
        let Some(child_plan) = plan.as_mut() else {
            panic!("subquery has no plan");
        };

        for child_outer_query_ref in child_plan
            .table_references
            .outer_query_refs()
            .iter()
            .filter(|t| t.is_used())
        {
            if let Some(joined_table) =
                table_refs.find_joined_table_by_internal_id_mut(child_outer_query_ref.internal_id)
            {
                joined_table.col_used_mask |= &child_outer_query_ref.col_used_mask;
            }
            if let Some(outer_query_ref) = table_refs
                .find_outer_query_ref_by_internal_id_mut(child_outer_query_ref.internal_id)
            {
                outer_query_ref.col_used_mask |= &child_outer_query_ref.col_used_mask;
            }
        }
    }
}

/// Emit the subqueries contained in the FROM clause.
/// This is done first so the results can be read in the main query loop.
pub fn emit_from_clause_subqueries(
    program: &mut ProgramBuilder,
    t_ctx: &mut TranslateCtx,
    tables: &mut TableReferences,
) -> Result<()> {
    if tables.joined_tables().is_empty() {
        emit_explain!(program, false, "SCAN CONSTANT ROW".to_owned());
    }

    for table_reference in tables.joined_tables_mut() {
        emit_explain!(
            program,
            true,
            match &table_reference.op {
                Operation::Scan(scan) => {
                    let table_name =
                        if table_reference.table.get_name() == table_reference.identifier {
                            table_reference.identifier.clone()
                        } else {
                            format!(
                                "{} AS {}",
                                table_reference.table.get_name(),
                                table_reference.identifier
                            )
                        };

                    match scan {
                        Scan::BTreeTable { index, .. } => {
                            if let Some(index) = index {
                                if table_reference.utilizes_covering_index() {
                                    format!("SCAN {table_name} USING COVERING INDEX {}", index.name)
                                } else {
                                    format!("SCAN {table_name} USING INDEX {}", index.name)
                                }
                            } else {
                                format!("SCAN {table_name}")
                            }
                        }
                        Scan::VirtualTable { .. } | Scan::Subquery | Scan::RecursiveCte => {
                            format!("SCAN {table_name}")
                        }
                    }
                }
                Operation::Search(search) => match search {
                    Search::RowidEq { .. } | Search::Seek { index: None, .. } => {
                        format!(
                            "SEARCH {} USING INTEGER PRIMARY KEY (rowid=?)",
                            table_reference.identifier
                        )
                    }
                    Search::Seek {
                        index: Some(index), ..
                    } => {
                        format!(
                            "SEARCH {} USING INDEX {}",
                            table_reference.identifier, index.name
                        )
                    }
                },
                Operation::IndexMethodQuery(query) => {
                    let index_method = query.index.index_method.as_ref().unwrap();
                    format!(
                        "QUERY INDEX METHOD {}",
                        index_method.definition().method_name
                    )
                }
                Operation::HashJoin(_) => "HASH JOIN".to_string(),
            }
        );

        if let Table::FromClauseSubquery(from_clause_subquery) = &mut table_reference.table {
            // Emit the subquery and get the start register of the result columns.
            let result_columns_start =
                emit_from_clause_subquery(program, &mut from_clause_subquery.plan, t_ctx)?;
            // Set the start register of the subquery's result columns.
            // This is done so that translate_expr() can read the result columns of the subquery,
            // as if it were reading from a regular table.
            from_clause_subquery.result_columns_start_reg = Some(result_columns_start);
        } else if let Table::RecursiveCte(recursive_cte) = &table_reference.table {
            // Emit the recursive CTE and populate the ephemeral output table.
            // Create a proper BTreeTable for the cursor type so OpenEphemeral knows the column count.
            use crate::schema::BTreeTable;
            let ephemeral_table = std::sync::Arc::new(BTreeTable {
                root_page: 0,
                name: format!("cte_{}", recursive_cte.name),
                primary_key_columns: vec![],
                columns: recursive_cte.columns.clone(),
                has_rowid: true,
                is_strict: false,
                has_autoincrement: false,
                unique_sets: vec![],
                foreign_keys: vec![],
            });
            let output_cursor_id = program.alloc_cursor_id_keyed_if_not_exists(
                CursorKey::table(table_reference.internal_id),
                CursorType::BTreeTable(ephemeral_table),
            );
            emit_recursive_cte(
                program,
                recursive_cte.clone(),
                output_cursor_id,
                table_reference.internal_id,
                t_ctx,
            )?;
        }

        program.pop_current_parent_explain();
    }
    Ok(())
}

/// Emit a FROM clause subquery and return the start register of the result columns.
/// This is done by emitting a coroutine that stores the result columns in sequential registers.
/// Each FROM clause subquery has its own separate SelectPlan which is wrapped in a coroutine.
///
/// The resulting bytecode from a subquery is mostly exactly the same as a regular query, except:
/// - it ends in an EndCoroutine instead of a Halt.
/// - instead of emitting ResultRows, the coroutine yields to the main query loop.
/// - the first register of the result columns is returned to the parent query,
///   so that translate_expr() can read the result columns of the subquery,
///   as if it were reading from a regular table.
///
/// Since a subquery has its own SelectPlan, it can contain nested subqueries,
/// which can contain even more nested subqueries, etc.
pub fn emit_from_clause_subquery(
    program: &mut ProgramBuilder,
    plan: &mut SelectPlan,
    t_ctx: &mut TranslateCtx,
) -> Result<usize> {
    let yield_reg = program.alloc_register();
    let coroutine_implementation_start_offset = program.allocate_label();
    match &mut plan.query_destination {
        QueryDestination::CoroutineYield {
            yield_reg: y,
            coroutine_implementation_start,
        } => {
            // The parent query will use this register to jump to/from the subquery.
            *y = yield_reg;
            // The parent query will use this register to reinitialize the coroutine when it needs to run multiple times.
            *coroutine_implementation_start = coroutine_implementation_start_offset;
        }
        _ => unreachable!("emit_from_clause_subquery called on non-subquery"),
    }
    let end_coroutine_label = program.allocate_label();
    let mut metadata = TranslateCtx {
        labels_main_loop: (0..plan.joined_tables().len())
            .map(|_| LoopLabels::new(program))
            .collect(),
        label_main_loop_end: None,
        meta_group_by: None,
        meta_left_joins: (0..plan.joined_tables().len()).map(|_| None).collect(),
        meta_sort: None,
        reg_agg_start: None,
        reg_nonagg_emit_once_flag: None,
        reg_result_cols_start: None,
        limit_ctx: None,
        reg_offset: None,
        reg_limit_offset_sum: None,
        resolver: Resolver::new(t_ctx.resolver.schema, t_ctx.resolver.symbol_table),
        non_aggregate_expressions: Vec::new(),
        cdc_cursor_id: None,
        meta_window: None,
        hash_table_contexts: std::collections::HashMap::new(),
    };
    let subquery_body_end_label = program.allocate_label();
    program.emit_insn(Insn::InitCoroutine {
        yield_reg,
        jump_on_definition: subquery_body_end_label,
        start_offset: coroutine_implementation_start_offset,
    });
    program.preassign_label_to_next_insn(coroutine_implementation_start_offset);
    let result_column_start_reg = emit_query(program, plan, &mut metadata)?;
    program.resolve_label(end_coroutine_label, program.offset());
    program.emit_insn(Insn::EndCoroutine { yield_reg });
    program.preassign_label_to_next_insn(subquery_body_end_label);
    Ok(result_column_start_reg)
}

/// Helper function to bind CTE column references in expressions.
/// This converts Expr::Id and Expr::Qualified that reference CTE columns
/// into Expr::Column with the appropriate internal_id and column index.
fn bind_cte_columns_in_expr(
    expr: &mut ast::Expr,
    cte_name: &str,
    column_names: &[String],
    internal_id: ast::TableInternalId,
) {
    use crate::util::normalize_ident;

    match expr {
        ast::Expr::Id(name) => {
            let normalized = normalize_ident(name.as_str());
            if let Some(col_idx) = column_names.iter().position(|c| c.eq_ignore_ascii_case(&normalized)) {
                *expr = ast::Expr::Column {
                    database: None,
                    table: internal_id,
                    column: col_idx,
                    is_rowid_alias: false,
                };
            }
        }
        ast::Expr::Qualified(table, name) => {
            let normalized_table = normalize_ident(table.as_str());
            let normalized_col = normalize_ident(name.as_str());
            if normalized_table.eq_ignore_ascii_case(cte_name) {
                if let Some(col_idx) = column_names.iter().position(|c| c.eq_ignore_ascii_case(&normalized_col)) {
                    *expr = ast::Expr::Column {
                        database: None,
                        table: internal_id,
                        column: col_idx,
                        is_rowid_alias: false,
                    };
                }
            }
        }
        // Recursively process sub-expressions
        ast::Expr::Binary(lhs, _, rhs) => {
            bind_cte_columns_in_expr(lhs, cte_name, column_names, internal_id);
            bind_cte_columns_in_expr(rhs, cte_name, column_names, internal_id);
        }
        ast::Expr::Unary(_, operand) => {
            bind_cte_columns_in_expr(operand, cte_name, column_names, internal_id);
        }
        ast::Expr::Parenthesized(exprs) => {
            for e in exprs {
                bind_cte_columns_in_expr(e, cte_name, column_names, internal_id);
            }
        }
        ast::Expr::Between { lhs, start, end, .. } => {
            bind_cte_columns_in_expr(lhs, cte_name, column_names, internal_id);
            bind_cte_columns_in_expr(start, cte_name, column_names, internal_id);
            bind_cte_columns_in_expr(end, cte_name, column_names, internal_id);
        }
        ast::Expr::Case { base, when_then_pairs, else_expr, .. } => {
            if let Some(b) = base {
                bind_cte_columns_in_expr(b, cte_name, column_names, internal_id);
            }
            for (when_expr, then_expr) in when_then_pairs {
                bind_cte_columns_in_expr(when_expr, cte_name, column_names, internal_id);
                bind_cte_columns_in_expr(then_expr, cte_name, column_names, internal_id);
            }
            if let Some(e) = else_expr {
                bind_cte_columns_in_expr(e, cte_name, column_names, internal_id);
            }
        }
        ast::Expr::Cast { expr: inner, .. } => {
            bind_cte_columns_in_expr(inner, cte_name, column_names, internal_id);
        }
        ast::Expr::Collate(inner, _) => {
            bind_cte_columns_in_expr(inner, cte_name, column_names, internal_id);
        }
        ast::Expr::FunctionCall { args, filter_over, .. } => {
            for arg in args {
                bind_cte_columns_in_expr(arg, cte_name, column_names, internal_id);
            }
            if let Some(f) = &mut filter_over.filter_clause {
                bind_cte_columns_in_expr(f, cte_name, column_names, internal_id);
            }
            if let Some(ast::Over::Window(w)) = &mut filter_over.over_clause {
                for e in &mut w.partition_by {
                    bind_cte_columns_in_expr(e, cte_name, column_names, internal_id);
                }
                for sorted_col in &mut w.order_by {
                    bind_cte_columns_in_expr(&mut sorted_col.expr, cte_name, column_names, internal_id);
                }
            }
        }
        ast::Expr::InList { lhs, rhs, .. } => {
            bind_cte_columns_in_expr(lhs, cte_name, column_names, internal_id);
            for e in rhs {
                bind_cte_columns_in_expr(e, cte_name, column_names, internal_id);
            }
        }
        ast::Expr::IsNull(inner) => {
            bind_cte_columns_in_expr(inner, cte_name, column_names, internal_id);
        }
        ast::Expr::NotNull(inner) => {
            bind_cte_columns_in_expr(inner, cte_name, column_names, internal_id);
        }
        // Literals, Column (already bound), and other terminals don't need processing
        _ => {}
    }
}

/// Helper function to bind table column references in expressions.
/// This converts Expr::Id and Expr::Qualified that reference columns of known tables
/// into Expr::Column with the appropriate internal_id and column index.
fn bind_table_columns_in_expr(
    expr: &mut ast::Expr,
    tables: &[(&str, ast::TableInternalId, &crate::schema::BTreeTable)],
) {
    use crate::util::normalize_ident;

    match expr {
        ast::Expr::Id(name) => {
            // Try to find this column in any of the tables
            let normalized = normalize_ident(name.as_str());
            for (_, internal_id, btree_table) in tables {
                if let Some(col_idx) = btree_table.columns.iter().position(|c| {
                    c.name.as_ref().map(|n| n.eq_ignore_ascii_case(&normalized)).unwrap_or(false)
                }) {
                    let is_rowid_alias = btree_table.columns[col_idx].is_rowid_alias();
                    *expr = ast::Expr::Column {
                        database: None,
                        table: *internal_id,
                        column: col_idx,
                        is_rowid_alias,
                    };
                    return;
                }
            }
        }
        ast::Expr::Qualified(table, name) => {
            let table_normalized = normalize_ident(table.as_str());
            let col_normalized = normalize_ident(name.as_str());
            for (table_name, internal_id, btree_table) in tables {
                if table_name.eq_ignore_ascii_case(&table_normalized) {
                    if let Some(col_idx) = btree_table.columns.iter().position(|c| {
                        c.name.as_ref().map(|n| n.eq_ignore_ascii_case(&col_normalized)).unwrap_or(false)
                    }) {
                        let is_rowid_alias = btree_table.columns[col_idx].is_rowid_alias();
                        *expr = ast::Expr::Column {
                            database: None,
                            table: *internal_id,
                            column: col_idx,
                            is_rowid_alias,
                        };
                        return;
                    }
                }
            }
        }
        // Recursive cases for compound expressions
        ast::Expr::Binary(lhs, _, rhs) => {
            bind_table_columns_in_expr(lhs, tables);
            bind_table_columns_in_expr(rhs, tables);
        }
        ast::Expr::Unary(_, inner) => {
            bind_table_columns_in_expr(inner, tables);
        }
        ast::Expr::Parenthesized(inner) => {
            for e in inner {
                bind_table_columns_in_expr(e, tables);
            }
        }
        ast::Expr::Between { lhs, start, end, .. } => {
            bind_table_columns_in_expr(lhs, tables);
            bind_table_columns_in_expr(start, tables);
            bind_table_columns_in_expr(end, tables);
        }
        ast::Expr::Case { base, when_then_pairs, else_expr, .. } => {
            if let Some(op) = base {
                bind_table_columns_in_expr(op, tables);
            }
            for (when, then) in when_then_pairs {
                bind_table_columns_in_expr(when, tables);
                bind_table_columns_in_expr(then, tables);
            }
            if let Some(else_e) = else_expr {
                bind_table_columns_in_expr(else_e, tables);
            }
        }
        ast::Expr::Cast { expr: inner, .. } => {
            bind_table_columns_in_expr(inner, tables);
        }
        ast::Expr::Collate(inner, _) => {
            bind_table_columns_in_expr(inner, tables);
        }
        ast::Expr::FunctionCall { args, filter_over, .. } => {
            for arg in args {
                bind_table_columns_in_expr(arg, tables);
            }
            if let Some(filter) = &mut filter_over.filter_clause {
                bind_table_columns_in_expr(filter, tables);
            }
        }
        ast::Expr::InList { lhs, rhs, .. } => {
            bind_table_columns_in_expr(lhs, tables);
            for e in rhs {
                bind_table_columns_in_expr(e, tables);
            }
        }
        ast::Expr::IsNull(inner) => {
            bind_table_columns_in_expr(inner, tables);
        }
        ast::Expr::NotNull(inner) => {
            bind_table_columns_in_expr(inner, tables);
        }
        _ => {}
    }
}

/// Emit bytecode for a recursive CTE.
///
/// Recursive CTE execution pattern:
/// 1. Open ephemeral tables: output (already allocated), working, temp
/// 2. Execute anchor query, insert results into output and working
/// 3. Loop:
///    a. Execute recursive member (reading from working), insert into temp
///    b. If temp is empty, exit
///    c. Copy temp to output
///    d. Swap working and temp contents
///    e. Repeat
pub fn emit_recursive_cte(
    program: &mut ProgramBuilder,
    recursive_cte: RecursiveCte,
    output_cursor_id: CursorID,
    internal_id: ast::TableInternalId,
    t_ctx: &TranslateCtx,
) -> Result<()> {
    use super::expr::{translate_expr, translate_condition_expr, ConditionMetadata};
    use turso_parser::ast::ResultColumn;
    use crate::schema::BTreeTable;
    use std::sync::Arc;
    use std::borrow::Cow;

    let num_columns = recursive_cte.columns.len();

    // Build column name list for binding
    let column_names: Vec<String> = recursive_cte.columns.iter()
        .map(|c| c.name.clone().unwrap_or_default())
        .collect();

    // Create a pseudo BTreeTable for the ephemeral output table
    // This is needed so OpenEphemeral can know the column count
    let ephemeral_table = Arc::new(BTreeTable {
        root_page: 0, // Will be set by OpenEphemeral
        name: format!("cte_{}", recursive_cte.name),
        primary_key_columns: vec![],
        columns: recursive_cte.columns.clone(),
        has_rowid: true,
        is_strict: false,
        has_autoincrement: false,
        unique_sets: vec![],
        foreign_keys: vec![],
    });

    // Open the output ephemeral table
    program.emit_insn(Insn::OpenEphemeral {
        cursor_id: output_cursor_id,
        is_table: true,
    });

    // Allocate working and temp cursors with proper table type
    let working_cursor_id = program.alloc_cursor_id(CursorType::BTreeTable(ephemeral_table.clone()));
    let temp_cursor_id = program.alloc_cursor_id(CursorType::BTreeTable(ephemeral_table.clone()));

    program.emit_insn(Insn::OpenEphemeral {
        cursor_id: working_cursor_id,
        is_table: true,
    });
    program.emit_insn(Insn::OpenEphemeral {
        cursor_id: temp_cursor_id,
        is_table: true,
    });

    // Allocate registers for row data and helper registers
    let row_reg_start = program.alloc_registers(num_columns);
    let result_reg_start = program.alloc_registers(num_columns); // For recursive member results
    let record_reg = program.alloc_register();
    let rowid_reg = program.alloc_register();

    // === Execute anchor query using the pre-planned anchor_plan ===
    // The anchor_plan was prepared during query planning and contains all necessary
    // table references, conditions, and result columns already bound.
    if let Some(mut anchor_plan) = recursive_cte.anchor_plan {
        use super::plan::QueryDestination;

        // Create a custom destination that inserts into both output AND working tables
        // For now, we'll use a CoroutineYield-style approach where we emit the anchor
        // and manually insert each result row into both tables

        // Set up the anchor to yield rows via coroutine
        let yield_reg = program.alloc_register();
        let coroutine_start = program.allocate_label();
        let coroutine_end = program.allocate_label();
        let insert_loop_start = program.allocate_label();

        anchor_plan.query_destination = QueryDestination::CoroutineYield {
            yield_reg,
            coroutine_implementation_start: coroutine_start,
        };

        // Initialize the coroutine - jump over the coroutine body to the insertion loop
        program.emit_insn(Insn::InitCoroutine {
            yield_reg,
            jump_on_definition: insert_loop_start,
            start_offset: coroutine_start,
        });

        // Emit the coroutine body (the anchor query)
        program.preassign_label_to_next_insn(coroutine_start);

        // Create a new TranslateCtx for the anchor query
        let mut anchor_ctx = super::emitter::TranslateCtx::new(
            program,
            t_ctx.resolver.schema,
            t_ctx.resolver.symbol_table,
            anchor_plan.table_references.joined_tables().len(),
        );

        let anchor_result_reg = super::emitter::emit_query(program, &mut anchor_plan, &mut anchor_ctx)?;

        program.emit_insn(Insn::EndCoroutine { yield_reg });

        // Insertion loop - call the coroutine and insert each row
        program.preassign_label_to_next_insn(insert_loop_start);
        program.emit_insn(Insn::Yield {
            yield_reg,
            end_offset: coroutine_end,
        });

        // Copy result columns to our row registers
        for i in 0..num_columns {
            program.emit_insn(Insn::Copy {
                src_reg: anchor_result_reg + i,
                dst_reg: row_reg_start + i,
                extra_amount: 0,
            });
        }

        // Make record and insert into output
        program.emit_insn(Insn::MakeRecord {
            start_reg: row_reg_start,
            count: num_columns,
            dest_reg: record_reg,
            index_name: None,
            affinity_str: None,
        });
        program.emit_insn(Insn::NewRowid {
            cursor: output_cursor_id,
            rowid_reg,
            prev_largest_reg: 0,
        });
        program.emit_insn(Insn::Insert {
            cursor: output_cursor_id,
            key_reg: rowid_reg,
            record_reg,
            flag: crate::vdbe::insn::InsertFlags::new(),
            table_name: "cte_output".to_string(),
        });

        // Also insert into working table
        program.emit_insn(Insn::NewRowid {
            cursor: working_cursor_id,
            rowid_reg,
            prev_largest_reg: 0,
        });
        program.emit_insn(Insn::Insert {
            cursor: working_cursor_id,
            key_reg: rowid_reg,
            record_reg,
            flag: crate::vdbe::insn::InsertFlags::new(),
            table_name: "cte_working".to_string(),
        });

        // Go back to get the next anchor row
        program.emit_insn(Insn::Goto {
            target_pc: insert_loop_start,
        });

        program.preassign_label_to_next_insn(coroutine_end);
    } else {
        // Fallback for simple anchors without FROM clause (shouldn't happen with proper planning)
        return Err(crate::LimboError::ParseError(
            "Recursive CTE anchor_plan is missing".into(),
        ));
    }

    // === Extract and process recursive member ===
    let (recursive_columns, recursive_where, recursive_from) = match &recursive_cte.recursive_member {
        turso_parser::ast::OneSelect::Select { columns, where_clause, from, .. } => {
            (columns.clone(), where_clause.clone(), from.clone())
        }
        turso_parser::ast::OneSelect::Values(_) => {
            return Err(crate::LimboError::ParseError(
                "Recursive member cannot be VALUES".into(),
            ));
        }
    };

    // Parse the FROM clause to find tables other than the CTE
    // These tables need to be opened and joined with the CTE
    let mut other_tables: Vec<(String, Arc<crate::schema::BTreeTable>, CursorID, ast::TableInternalId)> = Vec::new();

    if let Some(ref from_clause) = recursive_from {
        // Helper function to extract table name from SelectTable
        fn get_table_name(select_table: &turso_parser::ast::SelectTable) -> Option<String> {
            match select_table {
                turso_parser::ast::SelectTable::Table(qualified_name, _, _) => {
                    Some(crate::util::normalize_ident(qualified_name.name.as_str()))
                }
                _ => None,
            }
        }

        // Process the main table in FROM clause
        if let Some(table_name) = get_table_name(&from_clause.select) {
            if !table_name.eq_ignore_ascii_case(&recursive_cte.name) {
                // This is a real table, not the CTE - look it up in schema
                match t_ctx.resolver.schema.get_table(&table_name) {
                    Some(table) => {
                        if let crate::schema::Table::BTree(btree_table) = table.as_ref() {
                            let table_internal_id = program.table_reference_counter.next();
                            let cursor_id = program.alloc_cursor_id(CursorType::BTreeTable(btree_table.clone()));
                            other_tables.push((table_name.clone(), btree_table.clone(), cursor_id, table_internal_id));
                        } else {
                            return Err(crate::LimboError::ParseError(
                                format!("Table '{}' in recursive member must be a regular table", table_name),
                            ));
                        }
                    }
                    None => {
                        return Err(crate::LimboError::ParseError(
                            format!("no such table: {}", table_name),
                        ));
                    }
                }
            }
        }

        // Process joined tables
        for join in &from_clause.joins {
            if let Some(table_name) = get_table_name(&join.table) {
                if !table_name.eq_ignore_ascii_case(&recursive_cte.name) {
                    // This is a real table, not the CTE
                    match t_ctx.resolver.schema.get_table(&table_name) {
                        Some(table) => {
                            if let crate::schema::Table::BTree(btree_table) = table.as_ref() {
                                let table_internal_id = program.table_reference_counter.next();
                                let cursor_id = program.alloc_cursor_id(CursorType::BTreeTable(btree_table.clone()));
                                other_tables.push((table_name.clone(), btree_table.clone(), cursor_id, table_internal_id));
                            } else {
                                return Err(crate::LimboError::ParseError(
                                    format!("Table '{}' in recursive member must be a regular table", table_name),
                                ));
                            }
                        }
                        None => {
                            return Err(crate::LimboError::ParseError(
                                format!("no such table: {}", table_name),
                            ));
                        }
                    }
                }
            }
        }

        // Currently, we only support one external table in the recursive member
        if other_tables.len() > 1 {
            return Err(crate::LimboError::ParseError(
                "Recursive CTEs with multiple external tables are not yet supported".into(),
            ));
        }
    }

    // Build TableReferences for the recursive member
    // Include both the CTE (represented by working table) and any other tables
    let mut joined_tables_for_recursive = Vec::new();

    // Add the CTE as a "table" - its columns come from the working cursor
    let cte_table_for_refs = crate::schema::Table::RecursiveCte(RecursiveCte {
        name: recursive_cte.name.clone(),
        columns: recursive_cte.columns.clone(),
        anchor: recursive_cte.anchor.clone(),
        recursive_member: recursive_cte.recursive_member.clone(),
        anchor_plan: None, // Not needed for table references
    });
    joined_tables_for_recursive.push(JoinedTable {
        op: Operation::default_scan_for(&cte_table_for_refs),
        table: cte_table_for_refs,
        identifier: recursive_cte.name.clone(),
        internal_id,
        join_info: None,
        col_used_mask: ColumnUsedMask::default(),
        column_use_counts: Vec::new(),
        expression_index_usages: Vec::new(),
        database_id: 0,
    });

    // Add other tables
    for (table_name, btree_table, cursor_id, table_internal_id) in &other_tables {
        let table = crate::schema::Table::BTree(btree_table.clone());
        joined_tables_for_recursive.push(JoinedTable {
            op: Operation::default_scan_for(&table),
            table,
            identifier: table_name.clone(),
            internal_id: *table_internal_id,
            join_info: None,
            col_used_mask: ColumnUsedMask::default(),
            column_use_counts: Vec::new(),
            expression_index_usages: Vec::new(),
            database_id: 0,
        });
    }

    let mut table_refs_for_recursive = TableReferences::new(joined_tables_for_recursive, vec![]);

    // Build a mapping of table names to their info for expression binding
    let mut table_name_to_info: Vec<(&str, ast::TableInternalId, &crate::schema::BTreeTable)> = Vec::new();
    for (table_name, btree_table, _, table_internal_id) in &other_tables {
        table_name_to_info.push((table_name.as_str(), *table_internal_id, btree_table.as_ref()));
    }

    // Bind and rewrite expressions using the table references
    let mut bound_recursive_exprs: Vec<ast::Expr> = Vec::new();
    for result_col in &recursive_columns {
        match result_col {
            ResultColumn::Expr(expr, _) => {
                let mut bound_expr = expr.as_ref().clone();
                // First bind CTE column references
                bind_cte_columns_in_expr(&mut bound_expr, &recursive_cte.name, &column_names, internal_id);
                // Then bind other table column references
                bind_table_columns_in_expr(&mut bound_expr, &table_name_to_info);
                bound_recursive_exprs.push(bound_expr);
            }
            ResultColumn::Star | ResultColumn::TableStar(_) => {
                return Err(crate::LimboError::ParseError(
                    "Recursive member cannot use *".into(),
                ));
            }
        }
    }

    // Bind WHERE clause if present
    let bound_where = if let Some(w) = recursive_where {
        let mut bound = w.as_ref().clone();
        bind_cte_columns_in_expr(&mut bound, &recursive_cte.name, &column_names, internal_id);
        bind_table_columns_in_expr(&mut bound, &table_name_to_info);
        Some(bound)
    } else {
        None
    };

    // === Open cursors for other tables ===
    for (_table_name, btree_table, cursor_id, _) in &other_tables {
        program.emit_insn(Insn::OpenRead {
            cursor_id: *cursor_id,
            root_page: btree_table.root_page,
            db: 0,
        });
    }

    // === Recursive loop ===
    let recursive_loop_start = program.allocate_label();
    let recursive_done = program.allocate_label();

    // Counter for recursion depth limit
    let depth_counter_reg = program.alloc_register();
    let max_depth_reg = program.alloc_register();

    program.emit_insn(Insn::Integer {
        value: 0,
        dest: depth_counter_reg,
    });

    program.preassign_label_to_next_insn(recursive_loop_start);

    // Check recursion depth
    program.emit_insn(Insn::Integer {
        value: RECURSIVE_CTE_MAX_ITERATIONS,
        dest: max_depth_reg,
    });
    program.emit_insn(Insn::Ge {
        lhs: depth_counter_reg,
        rhs: max_depth_reg,
        target_pc: recursive_done,
        flags: crate::vdbe::insn::CmpInsFlags::default(),
        collation: None,
    });
    program.emit_insn(Insn::AddImm {
        register: depth_counter_reg,
        value: 1,
    });

    // Rewind working table
    program.emit_insn(Insn::Rewind {
        cursor_id: working_cursor_id,
        pc_if_empty: recursive_done,
    });

    // Clear temp table
    program.emit_insn(Insn::ResetSorter {
        cursor_id: temp_cursor_id,
    });

    // Inner loop over working table (CTE rows)
    let cte_loop_start = program.allocate_label();
    let cte_loop_next = program.allocate_label();

    program.preassign_label_to_next_insn(cte_loop_start);

    // Read columns from working table into registers
    for i in 0..num_columns {
        program.emit_insn(Insn::Column {
            cursor_id: working_cursor_id,
            column: i,
            dest: row_reg_start + i,
            default: None,
        });
    }

    // Set up the expression cache so column references read from our registers
    // Create a mutable resolver copy for this scope
    let mut local_resolver = t_ctx.resolver.clone();
    local_resolver.enable_expr_to_reg_cache();

    // Add cache entries for each CTE column
    for i in 0..num_columns {
        let col_expr = ast::Expr::Column {
            database: None,
            table: internal_id,
            column: i,
            is_rowid_alias: false,
        };
        local_resolver.expr_to_reg_cache.push((Cow::Owned(col_expr), row_reg_start + i));
    }

    // Allocate registers for other table columns
    let mut table_column_regs: Vec<(ast::TableInternalId, usize)> = Vec::new();
    for (_table_name, btree_table, _, table_internal_id) in &other_tables {
        let num_cols = btree_table.columns.len();
        let col_start_reg = program.alloc_registers(num_cols);
        table_column_regs.push((*table_internal_id, col_start_reg));

        // Add cache entries for each column of this table
        for (i, col) in btree_table.columns.iter().enumerate() {
            let col_expr = ast::Expr::Column {
                database: None,
                table: *table_internal_id,
                column: i,
                is_rowid_alias: col.is_rowid_alias(),
            };
            local_resolver.expr_to_reg_cache.push((Cow::Owned(col_expr), col_start_reg + i));
        }
    }

    // The innermost loop label (used for WHERE clause skip and Next)
    let innermost_loop_next;

    // If there are other tables, emit nested loops over them
    // For now, support one other table (the common case)
    let other_table_loop_info: Option<(CursorID, BranchOffset, BranchOffset, usize)>;

    if let Some((_, btree_table, cursor_id, _)) = other_tables.first() {
        let other_loop_start = program.allocate_label();
        let other_loop_next = program.allocate_label();

        // Rewind the other table
        program.emit_insn(Insn::Rewind {
            cursor_id: *cursor_id,
            pc_if_empty: cte_loop_next,
        });

        program.preassign_label_to_next_insn(other_loop_start);

        // Read columns from the other table
        if let Some((_, col_start_reg)) = table_column_regs.first() {
            for (i, col) in btree_table.columns.iter().enumerate() {
                if col.is_rowid_alias() {
                    program.emit_insn(Insn::RowId {
                        cursor_id: *cursor_id,
                        dest: col_start_reg + i,
                    });
                } else {
                    program.emit_insn(Insn::Column {
                        cursor_id: *cursor_id,
                        column: i,
                        dest: col_start_reg + i,
                        default: None,
                    });
                }
            }
        }

        other_table_loop_info = Some((*cursor_id, other_loop_start, other_loop_next, btree_table.columns.len()));
        innermost_loop_next = other_loop_next;
    } else {
        other_table_loop_info = None;
        innermost_loop_next = cte_loop_next;
    }

    // Apply WHERE clause if present - skip to innermost next if condition fails
    if let Some(ref where_expr) = bound_where {
        translate_condition_expr(
            program,
            &table_refs_for_recursive,
            where_expr,
            ConditionMetadata {
                jump_if_condition_is_true: false,
                jump_target_when_true: innermost_loop_next,
                jump_target_when_false: innermost_loop_next,
                jump_target_when_null: innermost_loop_next,
            },
            &local_resolver,
        )?;
    }

    // Evaluate recursive member expressions into result registers
    for (i, expr) in bound_recursive_exprs.iter().enumerate() {
        translate_expr(
            program,
            None, // No table references needed - we use the cache
            expr,
            result_reg_start + i,
            &local_resolver,
        )?;
    }

    // Make record and insert into temp
    program.emit_insn(Insn::MakeRecord {
        start_reg: result_reg_start,
        count: num_columns,
        dest_reg: record_reg,
        index_name: None,
        affinity_str: None,
    });
    program.emit_insn(Insn::NewRowid {
        cursor: temp_cursor_id,
        rowid_reg,
        prev_largest_reg: 0,
    });
    program.emit_insn(Insn::Insert {
        cursor: temp_cursor_id,
        key_reg: rowid_reg,
        record_reg,
        flag: crate::vdbe::insn::InsertFlags::new(),
        table_name: "cte_temp".to_string(),
    });

    // Close nested loops (inner to outer)
    // First close the other table loop if it exists
    if let Some((other_cursor_id, other_loop_start, other_loop_next, _)) = other_table_loop_info {
        program.preassign_label_to_next_insn(other_loop_next);
        program.emit_insn(Insn::Next {
            cursor_id: other_cursor_id,
            pc_if_next: other_loop_start,
        });
    }

    // Then close the CTE working table loop
    program.preassign_label_to_next_insn(cte_loop_next);
    program.emit_insn(Insn::Next {
        cursor_id: working_cursor_id,
        pc_if_next: cte_loop_start,
    });

    // Check if temp is empty
    program.emit_insn(Insn::Rewind {
        cursor_id: temp_cursor_id,
        pc_if_empty: recursive_done,
    });

    // Copy temp to output
    let copy_loop_start = program.allocate_label();
    program.preassign_label_to_next_insn(copy_loop_start);

    for i in 0..num_columns {
        program.emit_insn(Insn::Column {
            cursor_id: temp_cursor_id,
            column: i,
            dest: row_reg_start + i,
            default: None,
        });
    }
    program.emit_insn(Insn::MakeRecord {
        start_reg: row_reg_start,
        count: num_columns,
        dest_reg: record_reg,
        index_name: None,
        affinity_str: None,
    });
    program.emit_insn(Insn::NewRowid {
        cursor: output_cursor_id,
        rowid_reg,
        prev_largest_reg: 0,
    });
    program.emit_insn(Insn::Insert {
        cursor: output_cursor_id,
        key_reg: rowid_reg,
        record_reg,
        flag: crate::vdbe::insn::InsertFlags::new(),
        table_name: "cte_output".to_string(),
    });

    program.emit_insn(Insn::Next {
        cursor_id: temp_cursor_id,
        pc_if_next: copy_loop_start,
    });

    // Swap working and temp by clearing working and copying temp to it
    program.emit_insn(Insn::ResetSorter {
        cursor_id: working_cursor_id,
    });

    // Rewind temp for copying
    program.emit_insn(Insn::Rewind {
        cursor_id: temp_cursor_id,
        pc_if_empty: recursive_loop_start, // If temp empty, just continue (shouldn't happen here)
    });

    let swap_loop_start = program.allocate_label();
    program.preassign_label_to_next_insn(swap_loop_start);

    for i in 0..num_columns {
        program.emit_insn(Insn::Column {
            cursor_id: temp_cursor_id,
            column: i,
            dest: row_reg_start + i,
            default: None,
        });
    }
    program.emit_insn(Insn::MakeRecord {
        start_reg: row_reg_start,
        count: num_columns,
        dest_reg: record_reg,
        index_name: None,
        affinity_str: None,
    });
    program.emit_insn(Insn::NewRowid {
        cursor: working_cursor_id,
        rowid_reg,
        prev_largest_reg: 0,
    });
    program.emit_insn(Insn::Insert {
        cursor: working_cursor_id,
        key_reg: rowid_reg,
        record_reg,
        flag: crate::vdbe::insn::InsertFlags::new(),
        table_name: "cte_working".to_string(),
    });

    program.emit_insn(Insn::Next {
        cursor_id: temp_cursor_id,
        pc_if_next: swap_loop_start,
    });

    // Continue recursive loop
    program.emit_insn(Insn::Goto {
        target_pc: recursive_loop_start,
    });

    program.preassign_label_to_next_insn(recursive_done);

    Ok(())
}

/// Translate a subquery that is not part of the FROM clause.
/// If a subquery is uncorrelated (i.e. does not reference columns from the outer query),
/// it will be executed only once.
///
/// If it is correlated (i.e. references columns from the outer query),
/// it will be executed for each row of the outer query.
///
/// The result of the subquery is stored in:
///
/// - a single register for EXISTS subqueries,
/// - a range of registers for RowValue subqueries,
/// - an ephemeral index for IN subqueries.
pub fn emit_non_from_clause_subquery(
    program: &mut ProgramBuilder,
    t_ctx: &mut TranslateCtx,
    plan: SelectPlan,
    query_type: &SubqueryType,
    is_correlated: bool,
) -> Result<()> {
    program.incr_nesting();

    let label_skip_after_first_run = if !is_correlated {
        let label = program.allocate_label();
        program.emit_insn(Insn::Once {
            target_pc_when_reentered: label,
        });
        Some(label)
    } else {
        None
    };

    match query_type {
        SubqueryType::Exists { result_reg, .. } => {
            let subroutine_reg = program.alloc_register();
            program.emit_insn(Insn::BeginSubrtn {
                dest: subroutine_reg,
                dest_end: None,
            });
            program.emit_insn(Insn::Integer {
                value: 0,
                dest: *result_reg,
            });
            emit_program_for_select(program, &t_ctx.resolver, plan)?;
            program.emit_insn(Insn::Return {
                return_reg: subroutine_reg,
                can_fallthrough: true,
            });
        }
        SubqueryType::In { cursor_id } => {
            program.emit_insn(Insn::OpenEphemeral {
                cursor_id: *cursor_id,
                is_table: false,
            });
            emit_program_for_select(program, &t_ctx.resolver, plan)?;
        }
        SubqueryType::RowValue {
            result_reg_start,
            num_regs,
        } => {
            let subroutine_reg = program.alloc_register();
            program.emit_insn(Insn::BeginSubrtn {
                dest: subroutine_reg,
                dest_end: None,
            });
            for result_reg in *result_reg_start..*result_reg_start + *num_regs {
                program.emit_insn(Insn::Null {
                    dest: result_reg,
                    dest_end: None,
                });
            }
            emit_program_for_select(program, &t_ctx.resolver, plan)?;
            program.emit_insn(Insn::Return {
                return_reg: subroutine_reg,
                can_fallthrough: true,
            });
        }
    }
    if let Some(label) = label_skip_after_first_run {
        program.preassign_label_to_next_insn(label);
    }

    program.decr_nesting();
    Ok(())
}
