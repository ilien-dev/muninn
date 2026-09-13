(function_declaration name: (identifier) @name) @def.fn
(method_declaration name: (field_identifier) @name) @def.method
(type_declaration (type_spec name: (type_identifier) @name)) @def.type
(const_declaration (const_spec name: (identifier) @name)) @def.const
(source_file (var_declaration (var_spec name: (identifier) @name))) @def.var
