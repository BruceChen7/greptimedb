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

use std::sync::Arc;

use arrow_schema::SchemaRef as ArrowSchemaRef;
use common_catalog::consts::INFORMATION_SCHEMA_NAME;
use common_query::physical_plan::TaskContext;
use common_recordbatch::RecordBatch;
use datafusion::datasource::streaming::PartitionStream as DfPartitionStream;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter as DfRecordBatchStreamAdapter;
use datafusion::physical_plan::SendableRecordBatchStream as DfSendableRecordBatchStream;
use datatypes::prelude::{ConcreteDataType, ScalarVectorBuilder, VectorRef};
use datatypes::schema::{ColumnSchema, Schema, SchemaRef};
use datatypes::vectors::{StringVectorBuilder, UInt32VectorBuilder};
use snafu::ResultExt;
use table::metadata::TableType;

use crate::error::{CreateRecordBatchSnafu, Result};
use crate::information_schema::TABLES;
use crate::CatalogProviderRef;

pub(super) struct InformationSchemaTables {
    schema: SchemaRef,
    catalog_name: String,
    catalog_provider: CatalogProviderRef,
}

impl InformationSchemaTables {
    pub(super) fn new(catalog_name: String, catalog_provider: CatalogProviderRef) -> Self {
        let schema = Arc::new(Schema::new(vec![
            ColumnSchema::new("table_catalog", ConcreteDataType::string_datatype(), false),
            ColumnSchema::new("table_schema", ConcreteDataType::string_datatype(), false),
            ColumnSchema::new("table_name", ConcreteDataType::string_datatype(), false),
            ColumnSchema::new("table_type", ConcreteDataType::string_datatype(), false),
            ColumnSchema::new("table_id", ConcreteDataType::uint32_datatype(), true),
            ColumnSchema::new("engine", ConcreteDataType::string_datatype(), true),
        ]));
        Self {
            schema,
            catalog_name,
            catalog_provider,
        }
    }

    fn builder(&self) -> InformationSchemaTablesBuilder {
        InformationSchemaTablesBuilder::new(
            self.schema.clone(),
            self.catalog_name.clone(),
            self.catalog_provider.clone(),
        )
    }
}

/// Builds the `information_schema.TABLE` table row by row
///
/// Columns are based on <https://www.postgresql.org/docs/current/infoschema-columns.html>
struct InformationSchemaTablesBuilder {
    schema: SchemaRef,
    catalog_name: String,
    catalog_provider: CatalogProviderRef,

    catalog_names: StringVectorBuilder,
    schema_names: StringVectorBuilder,
    table_names: StringVectorBuilder,
    table_types: StringVectorBuilder,
    table_ids: UInt32VectorBuilder,
    engines: StringVectorBuilder,
}

impl InformationSchemaTablesBuilder {
    fn new(schema: SchemaRef, catalog_name: String, catalog_provider: CatalogProviderRef) -> Self {
        Self {
            schema,
            catalog_name,
            catalog_provider,
            catalog_names: StringVectorBuilder::with_capacity(42),
            schema_names: StringVectorBuilder::with_capacity(42),
            table_names: StringVectorBuilder::with_capacity(42),
            table_types: StringVectorBuilder::with_capacity(42),
            table_ids: UInt32VectorBuilder::with_capacity(42),
            engines: StringVectorBuilder::with_capacity(42),
        }
    }

    /// Construct the `information_schema.tables` virtual table
    async fn make_tables(&mut self) -> Result<RecordBatch> {
        let catalog_name = self.catalog_name.clone();

        for schema_name in self.catalog_provider.schema_names().await? {
            if schema_name == INFORMATION_SCHEMA_NAME {
                continue;
            }

            let Some(schema) = self.catalog_provider.schema(&schema_name).await? else { continue };
            for table_name in schema.table_names().await? {
                let Some(table) = schema.table(&table_name).await? else { continue };
                let table_info = table.table_info();
                self.add_table(
                    &catalog_name,
                    &schema_name,
                    &table_name,
                    table.table_type(),
                    Some(table_info.ident.table_id),
                    Some(&table_info.meta.engine),
                );
            }
        }

        // Add a final list for the information schema tables themselves
        self.add_table(
            &catalog_name,
            INFORMATION_SCHEMA_NAME,
            TABLES,
            TableType::View,
            None,
            None,
        );

        self.finish()
    }

    fn add_table(
        &mut self,
        catalog_name: &str,
        schema_name: &str,
        table_name: &str,
        table_type: TableType,
        table_id: Option<u32>,
        engine: Option<&str>,
    ) {
        self.catalog_names.push(Some(catalog_name));
        self.schema_names.push(Some(schema_name));
        self.table_names.push(Some(table_name));
        self.table_types.push(Some(match table_type {
            TableType::Base => "BASE TABLE",
            TableType::View => "VIEW",
            TableType::Temporary => "LOCAL TEMPORARY",
        }));
        self.table_ids.push(table_id);
        self.engines.push(engine);
    }

    fn finish(&mut self) -> Result<RecordBatch> {
        let columns: Vec<VectorRef> = vec![
            Arc::new(self.catalog_names.finish()),
            Arc::new(self.schema_names.finish()),
            Arc::new(self.table_names.finish()),
            Arc::new(self.table_types.finish()),
            Arc::new(self.table_ids.finish()),
            Arc::new(self.engines.finish()),
        ];
        RecordBatch::new(self.schema.clone(), columns).context(CreateRecordBatchSnafu)
    }
}

impl DfPartitionStream for InformationSchemaTables {
    fn schema(&self) -> &ArrowSchemaRef {
        self.schema.arrow_schema()
    }

    fn execute(&self, _: Arc<TaskContext>) -> DfSendableRecordBatchStream {
        let schema = self.schema().clone();
        let mut builder = self.builder();
        Box::pin(DfRecordBatchStreamAdapter::new(
            schema,
            futures::stream::once(async move {
                builder
                    .make_tables()
                    .await
                    .map(|x| x.into_df_record_batch())
                    .map_err(Into::into)
            }),
        ))
    }
}
