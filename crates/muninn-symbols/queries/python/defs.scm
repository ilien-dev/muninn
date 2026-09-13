(function_definition name: (identifier) @name) @def.fn
(class_definition name: (identifier) @name) @def.class
(module (expression_statement (assignment left: (identifier) @name (#match? @name "^[A-Z][A-Z0-9_]+$")))) @def.const
