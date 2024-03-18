use serde::{self, Serialize};

// Based on https://pkg.go.dev/k8s.io/apimachinery/pkg/apis/meta/v1#Table
// TODO(joonas): should we have a Table struct that can be used by multiple resource types?
// pub struct Table {
//    ...
//}

// Based on https://pkg.go.dev/k8s.io/apimachinery/pkg/apis/meta/v1#TableColumnDefinition
#[derive(Debug, Clone, Serialize)]
pub struct TableColumnDefinition {
    // name is a human readable name for the column.
    pub name: String,
    // type is an OpenAPI type definition for this column, such as number, integer, string, or
    // array.
    // See https://github.com/OAI/OpenAPI-Specification/blob/master/versions/2.0.md#data-types for more.
    #[serde(rename = "type")]
    pub kind: String,
    // format is an optional OpenAPI type modifier for this column. A format modifies the type and
    // imposes additional rules, like date or time formatting for a string. The 'name' format is applied
    // to the primary identifier column which has type 'string' to assist in clients identifying column
    // is the resource name.
    // See https://github.com/OAI/OpenAPI-Specification/blob/master/versions/2.0.md#data-types for more.
    pub format: String,
    // description is a human readable description of this column.
    pub description: String,
    // priority is an integer defining the relative importance of this column compared to others. Lower
    // numbers are considered higher priority. Columns that may be omitted in limited space scenarios
    // should be given a higher priority.
    /// NOTE: Set priority to 0 if you want things to show up in the non `-o wide` view, and anything above
    /// if you don't mind the value being hidden behind `-o wide` flag in kubectl get <resource>.
    pub priority: u8,
}

impl Default for TableColumnDefinition {
    fn default() -> Self {
        Self {
            name: "".to_string(),
            kind: "".to_string(),
            format: "".to_string(),
            description: "".to_string(),
            priority: 0,
        }
    }
}

// Based on https://pkg.go.dev/k8s.io/apimachinery/pkg/apis/meta/v1#TableRow
#[derive(Debug, Clone, Serialize)]
pub struct TableRow {
    // TODO(joonas): Support more than strings here
    // cells will be as wide as the column definitions array and may contain strings, numbers (float64 or
    // int64), booleans, simple maps, lists, or null. See the type field of the column definition for a
    // more detailed description.
    pub cells: Vec<String>,
}
