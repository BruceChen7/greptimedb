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

use std::collections::HashMap;

use api::helper::ColumnDataTypeWrapper;
use api::v1::{Column, DeleteRequest as GrpcDeleteRequest};
use datatypes::data_type::DataType;
use datatypes::prelude::ConcreteDataType;
use snafu::{ensure, ResultExt};
use table::requests::DeleteRequest;

use crate::error::{ColumnDataTypeSnafu, IllegalDeleteRequestSnafu, Result};
use crate::insert::add_values_to_builder;

pub fn to_table_delete_request(request: GrpcDeleteRequest) -> Result<DeleteRequest> {
    let row_count = request.row_count as usize;

    let mut key_column_values = HashMap::with_capacity(request.key_columns.len());
    for Column {
        column_name,
        values,
        null_mask,
        datatype,
        ..
    } in request.key_columns
    {
        let Some(values) = values else { continue };

        let datatype: ConcreteDataType = ColumnDataTypeWrapper::try_new(datatype)
            .context(ColumnDataTypeSnafu)?
            .into();

        let vector_builder = &mut datatype.create_mutable_vector(row_count);

        add_values_to_builder(vector_builder, values, row_count, null_mask)?;

        ensure!(
            key_column_values
                .insert(column_name.clone(), vector_builder.to_vector())
                .is_none(),
            IllegalDeleteRequestSnafu {
                reason: format!("Duplicated column '{column_name}' in delete request.")
            }
        );
    }

    Ok(DeleteRequest { key_column_values })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use api::v1::column::Values;
    use api::v1::ColumnDataType;
    use datatypes::prelude::{ScalarVector, VectorRef};
    use datatypes::vectors::{Int32Vector, StringVector};

    use super::*;

    #[test]
    fn test_to_table_delete_request() {
        let grpc_request = GrpcDeleteRequest {
            table_name: "foo".to_string(),
            region_number: 0,
            key_columns: vec![
                Column {
                    column_name: "id".to_string(),
                    values: Some(Values {
                        i32_values: vec![1, 2, 3],
                        ..Default::default()
                    }),
                    datatype: ColumnDataType::Int32 as i32,
                    ..Default::default()
                },
                Column {
                    column_name: "name".to_string(),
                    values: Some(Values {
                        string_values: vec!["a".to_string(), "b".to_string(), "c".to_string()],
                        ..Default::default()
                    }),
                    datatype: ColumnDataType::String as i32,
                    ..Default::default()
                },
            ],
            row_count: 3,
        };

        let mut request = to_table_delete_request(grpc_request).unwrap();

        assert_eq!(
            Arc::new(Int32Vector::from_slice(vec![1, 2, 3])) as VectorRef,
            request.key_column_values.remove("id").unwrap()
        );
        assert_eq!(
            Arc::new(StringVector::from_slice(&["a", "b", "c"])) as VectorRef,
            request.key_column_values.remove("name").unwrap()
        );
        assert!(request.key_column_values.is_empty());
    }
}
