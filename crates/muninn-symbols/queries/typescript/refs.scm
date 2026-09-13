(call_expression function: (identifier) @name) @ref.call
(call_expression function: (member_expression property: (property_identifier) @name)) @ref.call
(new_expression constructor: (identifier) @name) @ref.new
(import_statement source: (string) @name) @ref.import
(type_identifier) @name @ref.type
