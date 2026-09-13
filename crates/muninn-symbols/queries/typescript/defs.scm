(function_declaration name: (identifier) @name) @def.fn
(method_definition name: (property_identifier) @name) @def.method
(class_declaration name: (type_identifier) @name) @def.class
(abstract_class_declaration name: (type_identifier) @name) @def.class
(interface_declaration name: (type_identifier) @name) @def.interface
(type_alias_declaration name: (type_identifier) @name) @def.type
(enum_declaration name: (identifier) @name) @def.enum
(variable_declarator name: (identifier) @name value: (arrow_function)) @def.fn
(variable_declarator name: (identifier) @name value: (function_expression)) @def.fn
