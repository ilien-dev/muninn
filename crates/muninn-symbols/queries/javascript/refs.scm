(call_expression function: (identifier) @name) @ref.call
(call_expression function: (member_expression property: (property_identifier) @name)) @ref.call
(new_expression constructor: (identifier) @name) @ref.new
(import_statement source: (string) @name) @ref.import
(call_expression function: (identifier) @fn arguments: (arguments (string) @name) (#eq? @fn "require")) @ref.import
