(function_declaration name: (identifier) @name) @def.fn
(method_definition name: (property_identifier) @name) @def.method
(class_declaration name: (identifier) @name) @def.class
(variable_declarator name: (identifier) @name value: (arrow_function)) @def.fn
(variable_declarator name: (identifier) @name value: (function_expression)) @def.fn
(assignment_expression left: (member_expression property: (property_identifier) @name) right: (function_expression)) @def.method
(assignment_expression left: (member_expression property: (property_identifier) @name) right: (arrow_function)) @def.method
