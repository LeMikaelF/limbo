use std::fmt::Display;

use anyhow::Context;
use bitflags::bitflags;
use indexmap::IndexSet;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use sql_generation::model::{
    query::{
        Create, CreateIndex, Delete, Drop, DropIndex, Insert, Select,
        alter_table::{AlterTable, AlterTableType},
        select::{CompoundOperator, FromClause, ResultColumn, SelectInner, SelectTable},
        transaction::{Begin, Commit, Rollback},
        update::Update,
    },
    table::{Index, JoinTable, JoinType, SimValue, Table, TableContext},
};
use turso_parser::ast::Distinctness;

use crate::{generation::Shadow, runner::env::ShadowTablesMut};

// This type represents the potential queries on the database.
#[derive(Debug, Clone, Serialize, Deserialize, strum::EnumDiscriminants)]
pub enum Query {
    Create(Create),
    Select(Select),
    Insert(Insert),
    Delete(Delete),
    Update(Update),
    Drop(Drop),
    CreateIndex(CreateIndex),
    AlterTable(AlterTable),
    DropIndex(DropIndex),
    Begin(Begin),
    Commit(Commit),
    Rollback(Rollback),
    /// Placeholder query that still needs to be generated
    Placeholder,
}

impl Query {
    pub fn as_create(&self) -> &Create {
        match self {
            Self::Create(create) => create,
            _ => unreachable!(),
        }
    }

    pub fn unwrap_create(self) -> Create {
        match self {
            Self::Create(create) => create,
            _ => unreachable!(),
        }
    }

    #[inline]
    pub fn unwrap_insert(self) -> Insert {
        match self {
            Self::Insert(insert) => insert,
            _ => unreachable!(),
        }
    }

    pub fn dependencies(&self) -> IndexSet<String> {
        match self {
            Query::Select(select) => select.dependencies(),
            Query::Create(_) => IndexSet::new(),
            Query::Insert(Insert::Select { table, .. })
            | Query::Insert(Insert::Values { table, .. })
            | Query::Delete(Delete { table, .. })
            | Query::Update(Update { table, .. })
            | Query::Drop(Drop { table, .. })
            | Query::CreateIndex(CreateIndex {
                index: Index {
                    table_name: table, ..
                },
            })
            | Query::AlterTable(AlterTable {
                table_name: table, ..
            })
            | Query::DropIndex(DropIndex {
                table_name: table, ..
            }) => IndexSet::from_iter([table.clone()]),
            Query::Begin(_) | Query::Commit(_) | Query::Rollback(_) => IndexSet::new(),
            Query::Placeholder => IndexSet::new(),
        }
    }
    pub fn uses(&self) -> Vec<String> {
        match self {
            Query::Create(Create { table }) => vec![table.name.clone()],
            Query::Select(select) => select.dependencies().into_iter().collect(),
            Query::Insert(Insert::Select { table, .. })
            | Query::Insert(Insert::Values { table, .. })
            | Query::Delete(Delete { table, .. })
            | Query::Update(Update { table, .. })
            | Query::Drop(Drop { table, .. })
            | Query::CreateIndex(CreateIndex {
                index: Index {
                    table_name: table, ..
                },
            })
            | Query::AlterTable(AlterTable {
                table_name: table, ..
            })
            | Query::DropIndex(DropIndex {
                table_name: table, ..
            }) => vec![table.clone()],
            Query::Begin(..) | Query::Commit(..) | Query::Rollback(..) => vec![],
            Query::Placeholder => vec![],
        }
    }

    #[inline]
    pub fn is_transaction(&self) -> bool {
        matches!(
            self,
            Self::Begin(..) | Self::Commit(..) | Self::Rollback(..)
        )
    }

    #[inline]
    pub fn is_ddl(&self) -> bool {
        matches!(
            self,
            Self::Create(..)
                | Self::CreateIndex(..)
                | Self::Drop(..)
                | Self::AlterTable(..)
                | Self::DropIndex(..)
        )
    }
}

impl Display for Query {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create(create) => write!(f, "{create}"),
            Self::Select(select) => write!(f, "{select}"),
            Self::Insert(insert) => write!(f, "{insert}"),
            Self::Delete(delete) => write!(f, "{delete}"),
            Self::Update(update) => write!(f, "{update}"),
            Self::Drop(drop) => write!(f, "{drop}"),
            Self::CreateIndex(create_index) => write!(f, "{create_index}"),
            Self::AlterTable(alter_table) => write!(f, "{alter_table}"),
            Self::DropIndex(drop_index) => write!(f, "{drop_index}"),
            Self::Begin(begin) => write!(f, "{begin}"),
            Self::Commit(commit) => write!(f, "{commit}"),
            Self::Rollback(rollback) => write!(f, "{rollback}"),
            Self::Placeholder => Ok(()),
        }
    }
}

impl Shadow for Query {
    type Result = anyhow::Result<Vec<Vec<SimValue>>>;

    fn shadow(&self, env: &mut ShadowTablesMut) -> Self::Result {
        match self {
            Query::Create(create) => create.shadow(env),
            Query::Insert(insert) => insert.shadow(env),
            Query::Delete(delete) => delete.shadow(env),
            Query::Select(select) => select.shadow(env),
            Query::Update(update) => update.shadow(env),
            Query::Drop(drop) => drop.shadow(env),
            Query::CreateIndex(create_index) => Ok(create_index.shadow(env)),
            Query::AlterTable(alter_table) => alter_table.shadow(env),
            Query::DropIndex(drop_index) => drop_index.shadow(env),
            Query::Begin(begin) => Ok(begin.shadow(env)),
            Query::Commit(commit) => Ok(commit.shadow(env)),
            Query::Rollback(rollback) => Ok(rollback.shadow(env)),
            Query::Placeholder => Ok(vec![]),
        }
    }
}

bitflags! {
    pub struct QueryCapabilities: u32 {
        const CREATE = 1 << 0;
        const SELECT = 1 << 1;
        const INSERT = 1 << 2;
        const DELETE = 1 << 3;
        const UPDATE = 1 << 4;
        const DROP = 1 << 5;
        const CREATE_INDEX = 1 << 6;
        const ALTER_TABLE = 1 << 7;
        const DROP_INDEX = 1 << 8;
    }
}

impl QueryCapabilities {
    // TODO: can be const fn in the future
    pub fn from_list_queries(queries: &[QueryDiscriminants]) -> Self {
        queries
            .iter()
            .fold(Self::empty(), |accum, q| accum.union(q.into()))
    }
}

impl From<&QueryDiscriminants> for QueryCapabilities {
    fn from(value: &QueryDiscriminants) -> Self {
        (*value).into()
    }
}

impl From<QueryDiscriminants> for QueryCapabilities {
    fn from(value: QueryDiscriminants) -> Self {
        match value {
            QueryDiscriminants::Create => Self::CREATE,
            QueryDiscriminants::Select => Self::SELECT,
            QueryDiscriminants::Insert => Self::INSERT,
            QueryDiscriminants::Delete => Self::DELETE,
            QueryDiscriminants::Update => Self::UPDATE,
            QueryDiscriminants::Drop => Self::DROP,
            QueryDiscriminants::CreateIndex => Self::CREATE_INDEX,
            QueryDiscriminants::AlterTable => Self::ALTER_TABLE,
            QueryDiscriminants::DropIndex => Self::DROP_INDEX,
            QueryDiscriminants::Begin
            | QueryDiscriminants::Commit
            | QueryDiscriminants::Rollback => {
                unreachable!("QueryCapabilities do not apply to transaction queries")
            }
            QueryDiscriminants::Placeholder => {
                unreachable!("QueryCapabilities do not apply to query Placeholder")
            }
        }
    }
}

impl QueryDiscriminants {
    pub const ALL_NO_TRANSACTION: &[QueryDiscriminants] = &[
        QueryDiscriminants::Select,
        QueryDiscriminants::Create,
        QueryDiscriminants::Insert,
        QueryDiscriminants::Update,
        QueryDiscriminants::Delete,
        QueryDiscriminants::Drop,
        QueryDiscriminants::CreateIndex,
        QueryDiscriminants::AlterTable,
        QueryDiscriminants::DropIndex,
    ];
}

impl Shadow for Create {
    type Result = anyhow::Result<Vec<Vec<SimValue>>>;

    fn shadow(&self, tables: &mut ShadowTablesMut) -> Self::Result {
        if !tables.iter().any(|t| t.name == self.table.name) {
            tables.push(self.table.clone());
            Ok(vec![])
        } else {
            Err(anyhow::anyhow!(
                "Table {} already exists. CREATE TABLE statement ignored.",
                self.table.name
            ))
        }
    }
}

impl Shadow for CreateIndex {
    type Result = Vec<Vec<SimValue>>;
    fn shadow(&self, env: &mut ShadowTablesMut) -> Vec<Vec<SimValue>> {
        env.iter_mut()
            .find(|t| t.name == self.table_name)
            .unwrap()
            .indexes
            .push(self.index.clone());
        vec![]
    }
}

impl Shadow for Delete {
    type Result = anyhow::Result<Vec<Vec<SimValue>>>;

    fn shadow(&self, tables: &mut ShadowTablesMut) -> Self::Result {
        let table = tables.iter_mut().find(|t| t.name == self.table);

        if let Some(table) = table {
            // If the table exists, we can delete from it
            let t2 = table.clone();
            table.rows.retain_mut(|r| !self.predicate.test(r, &t2));
        } else {
            // If the table does not exist, we return an error
            return Err(anyhow::anyhow!(
                "Table {} does not exist. DELETE statement ignored.",
                self.table
            ));
        }

        Ok(vec![])
    }
}

impl Shadow for Drop {
    type Result = anyhow::Result<Vec<Vec<SimValue>>>;

    fn shadow(&self, tables: &mut ShadowTablesMut) -> Self::Result {
        tracing::info!("dropping {:?}", self);
        if !tables.iter().any(|t| t.name == self.table) {
            // If the table does not exist, we return an error
            return Err(anyhow::anyhow!(
                "Table {} does not exist. DROP statement ignored.",
                self.table
            ));
        }

        tables.retain(|t| t.name != self.table);

        Ok(vec![])
    }
}

impl Shadow for Insert {
    type Result = anyhow::Result<Vec<Vec<SimValue>>>;

    fn shadow(&self, tables: &mut ShadowTablesMut) -> Self::Result {
        match self {
            Insert::Values { table, values } => {
                let target = tables
                    .iter_mut()
                    .find(|t| t.name == *table)
                    .context("target table not found")?;

                let w = target.columns.len();
                for v in values {
                    anyhow::ensure!(v.len() == w, "VALUES arity != target width");
                    target.rows.push(v.clone());
                }
                Ok(vec![])
            }

            Insert::Select { table, select } => {
                let target_idx = tables
                    .iter()
                    .position(|t| t.name == *table)
                    .context("target table not found")?;

                let target_w = tables[target_idx].columns.len();

                let out_rows: Vec<Vec<SimValue>> = select.shadow(&mut *tables)?;

                let out_w = out_rows.first().map(|r| r.len()).unwrap_or(0);
                anyhow::ensure!(
                    out_w == target_w,
                    "INSERT..SELECT width mismatch: select={}, target={}",
                    out_w,
                    target_w
                );

                println!(
                    "###### adding {} rows to the shadow model for an INSERT INTO ... SELECT",
                    out_rows.len()
                );

                for r in out_rows {
                    assert_eq!(r.len(), target_w);
                    tables[target_idx].rows.push(r);
                }

                Ok(vec![])
            }
        }
    }
}

impl Shadow for FromClause {
    type Result = anyhow::Result<JoinTable>;
    fn shadow(&self, tables: &mut ShadowTablesMut) -> Self::Result {
        let mut join_table = match &self.table {
            SelectTable::Table(table) => {
                let first_table = tables
                    .iter()
                    .find(|t| t.name == *table)
                    .context("Table not found")?;
                JoinTable {
                    tables: vec![first_table.clone()],
                    rows: first_table.rows.clone(),
                }
            }
            SelectTable::Select(select) => {
                let select_dependencies = select.dependencies();
                let result_tables = tables
                    .iter()
                    .filter(|shadow_table| select_dependencies.contains(shadow_table.name.as_str()))
                    .cloned()
                    .collect();
                let rows = select.shadow(tables)?;
                JoinTable {
                    tables: result_tables,
                    rows,
                }
            }
        };

        println!(
            "at the beginning, rows have length {:?}",
            join_table
                .rows()
                .iter()
                .map(|r| r.len())
                .unique()
                .collect_vec()
        );

        for join in &self.joins {
            println!("join loop executed!");
            let joined_table = tables
                .iter()
                .find(|t| t.name == join.table)
                .context("Joined table not found")?;

            join_table.tables.push(joined_table.clone());

            match join.join_type {
                JoinType::Inner => {
                    // take a cartesian product of the rows
                    let all_row_pairs = join_table
                        .rows
                        .clone()
                        .into_iter()
                        .cartesian_product(joined_table.rows.iter());

                    let mut rows: Vec<Vec<SimValue>> = Vec::new();
                    for (row1, row2) in all_row_pairs {
                        let row = row1.iter().chain(row2.iter()).cloned().collect::<Vec<_>>();

                        let is_in = join.on.test(&row, &join_table);

                        if is_in {
                            if let Some(last) = rows.iter().last()
                                && row.len() != last.len()
                            {
                                panic!("about to add row with inconsistent length");
                            }
                            rows.push(row);
                        }
                    }
                    join_table.rows = rows;
                }
                _ => todo!(),
            }
        }

        assert!(
            join_table.rows().iter().map(|r| r.len()).unique().count() <= 1,
            "shadow rows don't all have the same length! found {:?}. Self: {self:?}",
            join_table
                .rows()
                .iter()
                .map(|r| r.len())
                .unique()
                .collect_vec()
        );

        Ok(join_table)
    }
}

impl Shadow for SelectInner {
    type Result = anyhow::Result<JoinTable>;

    fn shadow(&self, env: &mut ShadowTablesMut) -> Self::Result {
        if let Some(from) = &self.from {
            let mut join_table = from.shadow(env)?;
            let col_count = join_table.columns().count();
            for row in &mut join_table.rows {
                assert_eq!(
                    row.len(),
                    col_count,
                    "Row length does not match column length after join"
                );
            }
            let join_clone = join_table.clone();

            join_table
                .rows
                .retain(|row| self.where_clause.test(row, &join_clone));

            if self.distinctness == Distinctness::Distinct {
                join_table.rows.sort_unstable();
                join_table.rows.dedup();
            }

            Ok(join_table)
        } else {
            assert!(
                self.columns
                    .iter()
                    .all(|col| matches!(col, ResultColumn::Expr(_)))
            );

            // If `WHERE` is false, just return an empty table
            if !self.where_clause.test(&[], &Table::anonymous(vec![])) {
                return Ok(JoinTable {
                    tables: Vec::new(),
                    rows: Vec::new(),
                });
            }

            // Compute the results of the column expressions and make a row
            let mut row = Vec::new();
            for col in &self.columns {
                match col {
                    ResultColumn::Expr(expr) => {
                        let value = expr.eval(&[], &Table::anonymous(vec![]));
                        if let Some(value) = value {
                            row.push(value);
                        } else {
                            return Err(anyhow::anyhow!(
                                "Failed to evaluate expression in free select ({})",
                                expr.0
                            ));
                        }
                    }
                    _ => unreachable!("Only expressions are allowed in free selects"),
                }
            }

            Ok(JoinTable {
                tables: Vec::new(),
                rows: vec![row],
            })
        }
    }
}

impl Shadow for Select {
    type Result = anyhow::Result<Vec<Vec<SimValue>>>;

    //TODO maybe this is under-producing rows?
    fn shadow(&self, env: &mut ShadowTablesMut) -> Self::Result {
        let first_result = self.body.select.shadow(env)?;

        let mut rows = first_result.rows;

        for compound in self.body.compounds.iter() {
            let compound_results = compound.select.shadow(env)?;

            match compound.operator {
                CompoundOperator::Union => {
                    // Union means we need to combine the results, removing duplicates
                    let mut new_rows = compound_results.rows;
                    new_rows.extend(rows.clone());
                    new_rows.sort_unstable();
                    new_rows.dedup();
                    rows = new_rows;
                }
                CompoundOperator::UnionAll => {
                    // Union all means we just concatenate the results
                    rows.extend(compound_results.rows.into_iter());
                }
            }
        }

        Ok(rows)
    }
}

impl Shadow for Begin {
    type Result = Vec<Vec<SimValue>>;
    fn shadow(&self, tables: &mut ShadowTablesMut) -> Self::Result {
        // FIXME: currently the snapshot is taken eagerly
        // this is wrong for Deffered transactions
        tables.create_snapshot();
        vec![]
    }
}

impl Shadow for Commit {
    type Result = Vec<Vec<SimValue>>;
    fn shadow(&self, tables: &mut ShadowTablesMut) -> Self::Result {
        tables.apply_snapshot();
        vec![]
    }
}

impl Shadow for Rollback {
    type Result = Vec<Vec<SimValue>>;
    fn shadow(&self, tables: &mut ShadowTablesMut) -> Self::Result {
        tables.delete_snapshot();
        vec![]
    }
}

impl Shadow for Update {
    type Result = anyhow::Result<Vec<Vec<SimValue>>>;

    fn shadow(&self, tables: &mut ShadowTablesMut) -> Self::Result {
        let table = tables.iter_mut().find(|t| t.name == self.table);

        let table = if let Some(table) = table {
            table
        } else {
            return Err(anyhow::anyhow!(
                "Table {} does not exist. UPDATE statement ignored.",
                self.table
            ));
        };

        let t2 = table.clone();
        for row in table
            .rows
            .iter_mut()
            .filter(|r| self.predicate.test(r, &t2))
        {
            for (column, set_value) in &self.set_values {
                if let Some((idx, _)) = table
                    .columns
                    .iter()
                    .enumerate()
                    .find(|(_, c)| &c.name == column)
                {
                    row[idx] = set_value.clone();
                }
            }
        }

        Ok(vec![])
    }
}

impl Shadow for AlterTable {
    type Result = anyhow::Result<Vec<Vec<SimValue>>>;

    fn shadow(&self, tables: &mut ShadowTablesMut<'_>) -> Self::Result {
        let table = tables
            .iter_mut()
            .find(|t| t.name == self.table_name)
            .ok_or_else(|| anyhow::anyhow!("Table {} does not exist", self.table_name))?;

        match &self.alter_table_type {
            AlterTableType::RenameTo { new_name } => {
                table.name = new_name.clone();
            }
            AlterTableType::AddColumn { column } => {
                table.columns.push(column.clone());
                table.rows.iter_mut().for_each(|row| {
                    row.push(SimValue(turso_core::Value::Null));
                });
            }
            AlterTableType::AlterColumn { old, new } => {
                let col = table.columns.iter_mut().find(|c| c.name == *old).unwrap();
                *col = new.clone();
                table.indexes.iter_mut().for_each(|index| {
                    index.columns.iter_mut().for_each(|(col_name, _)| {
                        if col_name == old {
                            *col_name = new.name.clone();
                        }
                    });
                });
            }
            AlterTableType::RenameColumn { old, new } => {
                let col = table.columns.iter_mut().find(|c| c.name == *old).unwrap();
                col.name = new.clone();
                table.indexes.iter_mut().for_each(|index| {
                    index.columns.iter_mut().for_each(|(col_name, _)| {
                        if col_name == old {
                            *col_name = new.clone();
                        }
                    });
                });
            }
            AlterTableType::DropColumn { column_name } => {
                let col_idx = table
                    .columns
                    .iter()
                    .position(|c| c.name == *column_name)
                    .unwrap();
                table.columns.remove(col_idx);
                table.rows.iter_mut().for_each(|row| {
                    row.remove(col_idx);
                });
            }
        };
        Ok(vec![])
    }
}

impl Shadow for DropIndex {
    type Result = anyhow::Result<Vec<Vec<SimValue>>>;

    fn shadow(&self, tables: &mut ShadowTablesMut<'_>) -> Self::Result {
        let table = tables
            .iter_mut()
            .find(|t| t.name == self.table_name)
            .ok_or_else(|| anyhow::anyhow!("Table {} does not exist", self.table_name))?;

        table
            .indexes
            .retain(|index| index.index_name != self.index_name);
        Ok(vec![])
    }
}

#[cfg(test)]
mod from_clause_tests {
    use sql_generation::model::{
        query::predicate::Predicate,
        table::{Column, ColumnType, JoinedTable},
    };
    use turso_parser::ast::{Expr, Name};

    use super::*;
    use crate::runner::env::ShadowTablesMut;

    #[test]
    fn test_shadow_from_clause() {
        let mut commited_tables = Vec::new();
        let mut transaction_tables = Option::None;

        let mut tables: ShadowTablesMut = ShadowTablesMut {
            commited_tables: &mut commited_tables,
            transaction_tables: &mut transaction_tables,
        };

        let table1 = Table {
            name: "table1".to_string(),
            columns: vec![
                ("id".to_string(), ColumnType::Integer),
                ("value".to_string(), ColumnType::Text),
            ]
            .into_iter()
            .map(|(name, column_type)| Column {
                name,
                column_type,
                constraints: vec![],
            })
            .collect(),
            rows: vec![
                vec![SimValue::int(1), SimValue::text("A".to_string())],
                vec![SimValue::int(2), SimValue::text("B".to_string())],
                vec![SimValue::int(4), SimValue::text("D".to_string())],
            ],
            indexes: vec![],
        };

        let table2 = Table {
            name: "table2".to_string(),
            columns: vec![
                ("id".to_string(), ColumnType::Integer),
                ("description".to_string(), ColumnType::Text),
            ]
            .into_iter()
            .map(|(name, column_type)| Column {
                name,
                column_type,
                constraints: vec![],
            })
            .collect(),
            rows: vec![
                vec![SimValue::int(1), SimValue::text("Desc A".to_string())],
                vec![SimValue::int(2), SimValue::text("Desc B".to_string())],
                vec![SimValue::int(3), SimValue::text("Desc C".to_string())],
            ],
            indexes: vec![],
        };

        tables.push(table1);
        tables.push(table2);

        // SELECT * FROM table1 INNER JOIN table2 ON table1.id = table2.id;
        let from_clause = FromClause {
            table: SelectTable::Table("table1".to_string()),
            joins: vec![JoinedTable {
                table: "table2".to_string(),
                join_type: JoinType::Inner,
                on: Predicate(turso_parser::ast::Expr::Binary(
                    Box::new(Expr::Qualified(
                        Name::from_string("table1"),
                        Name::from_string("id"),
                    )),
                    turso_parser::ast::Operator::Equals,
                    Box::new(turso_parser::ast::Expr::Qualified(
                        Name::from_string("table2"),
                        Name::from_string("id"),
                    )),
                )),
            }],
        };

        let result = from_clause.shadow(&mut tables).unwrap();

        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            result.rows[0],
            vec![
                SimValue::int(1),
                SimValue::text("A".to_string()),
                SimValue::int(1),
                SimValue::text("Desc A".to_string()),
            ]
        );
        assert_eq!(
            result.rows[1],
            vec![
                SimValue::int(2),
                SimValue::text("B".to_string()),
                SimValue::int(2),
                SimValue::text("Desc B".to_string()),
            ]
        );
    }
}
