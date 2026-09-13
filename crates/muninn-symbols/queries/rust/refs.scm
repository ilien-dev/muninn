(call_expression function: (identifier) @name) @ref.call
(call_expression function: (scoped_identifier name: (identifier) @name)) @ref.call
(call_expression function: (field_expression field: (field_identifier) @name)) @ref.call
(use_declaration argument: (_) @name) @ref.import
(macro_invocation macro: (identifier) @name) @ref.macro
(type_identifier) @name @ref.type
