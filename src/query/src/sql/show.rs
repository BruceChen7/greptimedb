// Copyright 2023 Greptime Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
use std::fmt::Display;

use datatypes::schema::{ColumnDefaultConstraint, ColumnSchema, SchemaRef, COMMENT_KEY};
use humantime::format_duration;
use snafu::ResultExt;
use sql::ast::{
    ColumnDef, ColumnOption, ColumnOptionDef, Expr, ObjectName, SqlOption, TableConstraint,
    Value as SqlValue,
};
use sql::dialect::GenericDialect;
use sql::parser::ParserContext;
use sql::statements::create::{CreateTable, TIME_INDEX};
use sql::statements::{self};
use table::metadata::{TableInfoRef, TableMeta};

use crate::error::{ConvertSqlTypeSnafu, ConvertSqlValueSnafu, Result, SqlSnafu};

#[inline]
fn number_value<T: Display>(n: T) -> SqlValue {
    SqlValue::Number(format!("{}", n), false)
}

#[inline]
fn string_value(s: impl Into<String>) -> SqlValue {
    SqlValue::SingleQuotedString(s.into())
}

#[inline]
fn sql_option(name: &str, value: SqlValue) -> SqlOption {
    SqlOption {
        name: name.into(),
        value,
    }
}

fn create_sql_options(table_meta: &TableMeta) -> Vec<SqlOption> {
    let table_opts = &table_meta.options;
    let mut options = Vec::with_capacity(4 + table_opts.extra_options.len());

    if !table_meta.region_numbers.is_empty() {
        options.push(sql_option(
            "regions",
            number_value(table_meta.region_numbers.len()),
        ));
    }

    if let Some(write_buffer_size) = table_opts.write_buffer_size {
        options.push(sql_option(
            "write_buffer_size",
            string_value(write_buffer_size.to_string()),
        ));
    }
    if let Some(ttl) = table_opts.ttl {
        options.push(sql_option(
            "ttl",
            string_value(format_duration(ttl).to_string()),
        ));
    }

    if let Some(w) = table_opts.compaction_time_window {
        options.push(sql_option("compaction_time_window", number_value(w)));
    }

    for (k, v) in &table_opts.extra_options {
        options.push(sql_option(k, string_value(v)));
    }

    options
}

#[inline]
fn column_option_def(option: ColumnOption) -> ColumnOptionDef {
    ColumnOptionDef { name: None, option }
}

fn create_column_def(column_schema: &ColumnSchema) -> Result<ColumnDef> {
    let name = &column_schema.name;
    let mut options = Vec::with_capacity(2);

    if column_schema.is_nullable() {
        options.push(column_option_def(ColumnOption::Null));
    } else {
        options.push(column_option_def(ColumnOption::NotNull));
    }

    if let Some(c) = column_schema.default_constraint() {
        let expr = match c {
            ColumnDefaultConstraint::Value(v) => Expr::Value(
                statements::value_to_sql_value(v)
                    .with_context(|_| ConvertSqlValueSnafu { value: v.clone() })?,
            ),
            ColumnDefaultConstraint::Function(expr) => {
                ParserContext::parse_function(expr, &GenericDialect {}).context(SqlSnafu)?
            }
        };

        options.push(column_option_def(ColumnOption::Default(expr)));
    }

    if let Some(c) = column_schema.metadata().get(COMMENT_KEY) {
        options.push(column_option_def(ColumnOption::Comment(c.to_string())));
    }

    Ok(ColumnDef {
        name: name[..].into(),
        data_type: statements::concrete_data_type_to_sql_data_type(&column_schema.data_type)
            .with_context(|_| ConvertSqlTypeSnafu {
                datatype: column_schema.data_type.clone(),
            })?,
        collation: None,
        options,
    })
}

fn create_table_constraints(schema: &SchemaRef, table_meta: &TableMeta) -> Vec<TableConstraint> {
    let mut constraints = Vec::with_capacity(2);
    if let Some(timestamp_column) = schema.timestamp_column() {
        let column_name = &timestamp_column.name;
        constraints.push(TableConstraint::Unique {
            name: Some(TIME_INDEX.into()),
            columns: vec![column_name[..].into()],
            is_primary: false,
        });
    }
    if !table_meta.primary_key_indices.is_empty() {
        let columns = table_meta
            .row_key_column_names()
            .map(|name| name[..].into())
            .collect();
        constraints.push(TableConstraint::Unique {
            name: None,
            columns,
            is_primary: true,
        });
    }

    constraints
}

/// Create a CreateTable statement from table info.
pub fn create_table_stmt(table_info: &TableInfoRef) -> Result<CreateTable> {
    let table_meta = &table_info.meta;
    let table_name = &table_info.name;
    let schema = &table_info.meta.schema;

    let columns = schema
        .column_schemas()
        .iter()
        .map(create_column_def)
        .collect::<Result<Vec<_>>>()?;

    let constraints = create_table_constraints(schema, table_meta);

    Ok(CreateTable {
        if_not_exists: true,
        table_id: table_info.ident.table_id,
        name: ObjectName(vec![table_name[..].into()]),
        columns,
        engine: table_meta.engine.clone(),
        constraints,
        options: create_sql_options(table_meta),
        partitions: None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use common_time::timestamp::TimeUnit;
    use datatypes::prelude::ConcreteDataType;
    use datatypes::schema::{Schema, SchemaRef};
    use table::metadata::*;

    use super::*;

    #[test]
    fn test_show_create_table_sql() {
        let schema = vec![
            ColumnSchema::new("id", ConcreteDataType::uint32_datatype(), true),
            ColumnSchema::new("host", ConcreteDataType::string_datatype(), true),
            ColumnSchema::new("cpu", ConcreteDataType::float64_datatype(), true),
            ColumnSchema::new("disk", ConcreteDataType::float32_datatype(), true),
            ColumnSchema::new(
                "ts",
                ConcreteDataType::timestamp_datatype(TimeUnit::Millisecond),
                false,
            )
            .with_default_constraint(Some(ColumnDefaultConstraint::Function(String::from(
                "current_timestamp()",
            ))))
            .unwrap()
            .with_time_index(true),
        ];
        let table_schema = SchemaRef::new(Schema::new(schema));
        let table_name = "system_metrics";
        let schema_name = "public".to_string();
        let catalog_name = "greptime".to_string();
        let regions = vec![0, 1, 2];

        let meta = TableMetaBuilder::default()
            .schema(table_schema)
            .primary_key_indices(vec![0, 1])
            .value_indices(vec![2, 3])
            .engine("mito".to_string())
            .next_column_id(0)
            .engine_options(Default::default())
            .options(Default::default())
            .created_on(Default::default())
            .region_numbers(regions)
            .build()
            .unwrap();

        let info = Arc::new(
            TableInfoBuilder::default()
                .table_id(1024)
                .table_version(0 as TableVersion)
                .name(table_name)
                .schema_name(schema_name)
                .catalog_name(catalog_name)
                .desc(None)
                .table_type(TableType::Base)
                .meta(meta)
                .build()
                .unwrap(),
        );

        let stmt = create_table_stmt(&info).unwrap();

        let sql = format!("\n{}", stmt);
        assert_eq!(
            r#"
CREATE TABLE IF NOT EXISTS system_metrics (
  id INT UNSIGNED NULL,
  host STRING NULL,
  cpu DOUBLE NULL,
  disk FLOAT NULL,
  ts TIMESTAMP(3) NOT NULL DEFAULT current_timestamp(),
  TIME INDEX (ts),
  PRIMARY KEY (id, host)
)
ENGINE=mito
WITH(
  regions = 3
)"#,
            sql
        );
    }
}
