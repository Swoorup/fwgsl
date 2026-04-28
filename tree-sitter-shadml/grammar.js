/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

// Tree-sitter grammar for shadml — a pure functional language for WebGPU.
//
// shadml uses Haskell-style indentation-sensitive layout. Since tree-sitter
// has limited indentation support, this grammar uses a simplified approach:
// it treats the language as mostly flat at the top level and uses regex-based
// heuristics for nested constructs. For precise indentation, editors should
// rely on the LSP semantic tokens.

module.exports = grammar({
  name: "shadml",

  extras: ($) => [/\s/, $.line_comment, $.block_comment],

  externals: ($) => [$._layout_end, $._layout_semicolon],

  word: ($) => $.identifier,

  conflicts: ($) => [
    [$.import_declaration],
    [$.constructor_expression, $.record_expression],
    [$.cfg_declaration],
    [$.function_declaration],
    [$.type_signature],
    [$.attribute],
    [$.type_constraint, $.type_constructor],
    [$.assoc_type_decl],
    [$.assoc_type_def],
    [$.immediate_declaration],
    [$.binding_declaration],
  ],

  rules: {
    source_file: ($) => repeat($._declaration),

    _declaration: ($) =>
      choice(
        $.module_declaration,
        $.import_declaration,
        $.data_declaration,
        $.type_alias,
        $.extern_declaration,
        $.binding_declaration,
        $.immediate_declaration,
        $.trait_declaration,
        $.impl_declaration,
        $.bitfield_declaration,
        $.const_declaration,
        $.cfg_declaration,
        $.type_signature,
        $.function_declaration,
        $.render_declaration,
        $.builtin_type_declaration,
        $.builtin_impl_declaration,
      ),

    // -- Module & imports ---------------------------------------------------

    module_declaration: ($) => seq("module", $.module_path),

    import_declaration: ($) =>
      seq(
        "import",
        $.module_path,
        optional(seq("as", $.upper_identifier)),
        optional(seq("when", $.cfg_predicate)),
      ),

    module_path: ($) =>
      seq($.upper_identifier, repeat(seq(".", $.upper_identifier))),

    // -- Data declarations --------------------------------------------------

    data_declaration: ($) =>
      seq(
        "data",
        field("name", $.upper_identifier),
        repeat($.type_variable),
        "=",
        $.constructor_list,
        optional(seq("deriving", $.deriving_list)),
        optional($._layout_end),
      ),

    constructor_list: ($) =>
      seq($.constructor, repeat(seq("|", $.constructor))),

    constructor: ($) =>
      prec.right(seq(
        field("name", $.upper_identifier),
        optional(choice(
          $.record_fields,
          repeat1($._simple_type),
        )),
        optional(seq("=", field("discriminant", $.integer_literal))),
      )),

    record_fields: ($) =>
      seq("{", commaSep1Trailing($.record_field), "}"),

    record_field: ($) =>
      seq(
        repeat($.attribute),
        field("name", $.identifier),
        ":",
        field("type", $._type),
      ),

    deriving_list: ($) =>
      seq("(", commaSep1($.upper_identifier), ")"),

    // -- Type alias ----------------------------------------------------------

    type_alias: ($) =>
      seq("alias", field("name", $.upper_identifier), "=", $._type, optional($._layout_end)),

    // -- Extern declarations --------------------------------------------------

    extern_declaration: ($) =>
      prec.right(seq(
        "extern",
        field("name", choice($.identifier, $.operator_name)),
        ":",
        field("type", $._type),
        optional($._layout_end),
      )),

    // -- Binding declarations (GPU resources) ---------------------------------

    // Both flat and grouped binding declarations.
    // Flat:    @group(N) @binding(N) uniform name : Type
    // Grouped: @group(N) \n @binding(N) uniform name : Type \n @binding(M) ...
    binding_declaration: ($) =>
      seq(
        "@", "group", "(", $.expression, ")",
        optional($._layout_semicolon),
        $.binding_entry,
        repeat(seq($._layout_semicolon, $.binding_entry)),
        optional($._layout_semicolon),
        optional($._layout_end),
      ),

    binding_entry: ($) =>
      seq(
        "@", "binding", "(", $.expression, ")",
        repeat($.attribute),
        field("space", optional(choice("uniform", seq("storage", optional(seq("(", $.identifier, ")")))))),
        field("name", $.identifier),
        ":",
        field("type", $._type),
      ),

    // -- Immediate (push constant) declarations -----------------------------

    immediate_declaration: ($) =>
      seq(
        "immediate",
        field("name", $.identifier),
        ":",
        field("type", $._type),
        optional($._layout_end),
      ),

    // -- Trait / impl --------------------------------------------------------

    trait_declaration: ($) =>
      prec.right(seq(
        "trait",
        field("name", $.upper_identifier),
        repeat($.type_variable),
        "where",
        repeat($._trait_member),
        optional($._layout_end),
      )),

    _trait_member: ($) =>
      choice($.type_signature, $.function_declaration, $.assoc_type_decl),

    assoc_type_decl: ($) =>
      seq("type", field("name", $.upper_identifier), optional(choice($._layout_semicolon, $._layout_end))),

    impl_declaration: ($) =>
      prec.right(seq(
        "impl",
        field("trait", $.upper_identifier),
        repeat($._simple_type),
        "where",
        repeat($._impl_member),
        optional($._layout_end),
      )),

    _impl_member: ($) =>
      choice($.type_signature, $.function_declaration, $.assoc_type_def),

    assoc_type_def: ($) =>
      seq("type", field("name", $.upper_identifier), "=", field("type", $._type), optional(choice($._layout_semicolon, $._layout_end))),

    // -- Bitfield ------------------------------------------------------------

    bitfield_declaration: ($) =>
      seq(
        "bitfield",
        field("name", $.upper_identifier),
        ":",
        field("backing", $._type),
        "=",
        field("constructor", $.upper_identifier),
        "{",
        commaSep1($.bitfield_field),
        optional(","),
        "}",
      ),

    bitfield_field: ($) =>
      seq(
        field("name", $.identifier),
        ":",
        choice(
          // Bare integer width: `name : 5`
          field("bits", $.integer_literal),
          // Typed with explicit width: `name : Type : 5`
          seq(
            field("type", $.upper_identifier),
            ":",
            field("bits", $.integer_literal),
          ),
          // Bool or enum-inferred: `name : Bool` or `name : CapStyle`
          field("type", $.upper_identifier),
        ),
      ),

    // -- Builtin declarations -----------------------------------------------

    builtin_type_declaration: ($) =>
      seq("builtin", "type", field("name", $.upper_identifier), optional(field("arity", $.integer_literal)), optional(choice($._layout_semicolon, $._layout_end))),

    builtin_impl_declaration: ($) =>
      prec.right(seq(
        "builtin", "impl",
        field("trait", $.upper_identifier),
        repeat($._simple_type),
        "where",
        repeat($._builtin_impl_member),
        optional($._layout_end),
      )),

    _builtin_impl_member: ($) =>
      choice($.assoc_type_def, $.builtin_method_def),

    builtin_method_def: ($) =>
      seq(
        field("name", choice($.identifier, $.operator_name)),
        "=",
        field("lowering", $.builtin_lowering),
        optional($._layout_semicolon),
      ),

    builtin_lowering: ($) =>
      choice(
        seq($.identifier, "(", $.identifier, ")"),
        seq($.identifier, $.operator_name),
      ),

    // -- Const ---------------------------------------------------------------

    const_declaration: ($) =>
      seq(
        "const",
        field("name", $.identifier),
        optional(seq(":", $._type)),
        "=",
        $.expression,
        optional(choice($._layout_semicolon, $._layout_end)),
      ),

    // -- Conditional compilation (when/cfg) ----------------------------------

    cfg_declaration: ($) =>
      prec.right(seq(
        "when", $.cfg_predicate, repeat($._declaration),
        optional(choice(
          seq("else", "when", $.cfg_predicate, repeat($._declaration)),
          seq("else", repeat($._declaration)),
        )),
      )),

    cfg_predicate: ($) =>
      choice(
        $.cfg_feature,
        $.cfg_not,
        $.cfg_and,
        $.cfg_or,
        seq("(", $.cfg_predicate, ")"),
      ),

    cfg_feature: ($) => seq("cfg", ".", $.identifier),
    cfg_not: ($) => prec(3, seq("not", $.cfg_predicate)),
    cfg_and: ($) => prec.left(2, seq($.cfg_predicate, "&&", $.cfg_predicate)),
    cfg_or: ($) => prec.left(1, seq($.cfg_predicate, "||", $.cfg_predicate)),

    // -- Type signatures and function declarations ---------------------------

    type_signature: ($) =>
      seq(
        repeat($.attribute),
        field("name", choice($.identifier, $.operator_name)),
        ":",
        field("type", $._type),
        optional(choice($._layout_semicolon, $._layout_end)),
      ),

    function_declaration: ($) =>
      seq(
        repeat($.attribute),
        field("name", choice($.identifier, $.operator_name)),
        repeat($.pattern),
        "=",
        field("body", $.expression),
        optional($.where_clause),
        optional(choice($._layout_semicolon, $._layout_end)),
      ),

    where_clause: ($) =>
      prec.right(seq("where", repeat1($.local_binding), optional($._layout_end))),

    local_binding: ($) =>
      prec.right(seq(
        field("name", $.identifier),
        repeat($.pattern),
        "=",
        $.expression,
        optional($._layout_semicolon),
      )),

    // -- Render declarations -------------------------------------------------

    render_declaration: ($) =>
      prec.right(seq(
        "render",
        field("name", $.identifier),
        repeat($._render_member),
        optional($._layout_end),
      )),

    _render_member: ($) =>
      choice(
        $.binding_declaration,
        $.immediate_declaration,
        $.type_signature,
        $.function_declaration,
      ),

    // -- Attributes ----------------------------------------------------------

    attribute: ($) =>
      seq(
        "@",
        $.identifier,
        optional(seq("(", commaSep1($.attr_arg), ")")),
      ),

    attr_arg: ($) =>
      choice(
        seq($.identifier, "=", $.attr_value),
        $.attr_value,
      ),

    attr_value: ($) =>
      choice(
        $.identifier,
        $.string_literal,
        $.integer_literal,
        $.float_literal,
      ),

    // -- Types ---------------------------------------------------------------

    _type: ($) =>
      choice(
        $.constrained_type,
        $.function_type,
        $.forall_type,
        $._simple_type,
      ),

    constrained_type: ($) =>
      prec.right(1, seq($.type_constraint, "=>", $._type)),

    type_constraint: ($) =>
      choice(
        seq($.upper_identifier, repeat1($.type_variable)),
        seq("(", commaSep1(seq($.upper_identifier, repeat1($.type_variable))), ")"),
      ),

    function_type: ($) =>
      prec.right(1, seq($._simple_type, "->", $._type)),

    forall_type: ($) =>
      seq("forall", repeat1($.type_variable), ".", $._type),

    _simple_type: ($) =>
      choice(
        $.type_constructor,
        $.type_variable,
        $.type_application,
        $.type_literal,
        $.tuple_type,
        $.unit_type,
        $.parenthesized_type,
        $.self_type,
        $.type_projection,
      ),

    type_literal: ($) => $.integer_literal,

    type_constructor: ($) => $.upper_identifier,

    self_type: ($) => "Self",

    type_variable: ($) => $.identifier,

    type_application: ($) =>
      choice(
        // Angle-bracket syntax: Vec<2, F32>
        seq($.upper_identifier, "<", commaSep1($._type), ">"),
        // Haskell-style space-separated: Box I32, Tensor 2 F32
        prec.left(3, seq(
          choice($.type_constructor, $.type_application),
          choice(
            $.type_constructor,
            $.type_variable,
            $.type_literal,
            $.tuple_type,
            $.unit_type,
            $.parenthesized_type,
            $.self_type,
            $.type_projection,
          ),
        )),
      ),

    tuple_type: ($) =>
      seq("(", $._type, ",", commaSep1($._type), ")"),

    unit_type: ($) => seq("(", ")"),

    parenthesized_type: ($) =>
      seq("(", $._type, ")"),

    type_projection: ($) =>
      prec.left(4, seq($._simple_type, ".", field("name", $.upper_identifier))),

    // -- Patterns ------------------------------------------------------------

    pattern: ($) =>
      choice(
        $.identifier_pattern,
        $.constructor_pattern,
        $.record_pattern,
        $.wildcard_pattern,
        $.literal_pattern,
        $.tuple_pattern,
        $.parenthesized_pattern,
      ),

    identifier_pattern: ($) => $.identifier,
    wildcard_pattern: ($) => "_",
    literal_pattern: ($) => $._literal,

    constructor_pattern: ($) =>
      prec.right(seq($.upper_identifier, repeat($.pattern))),

    record_pattern: ($) =>
      seq($.upper_identifier, "{", commaSepTrailing($.record_field_pattern), "}"),

    record_field_pattern: ($) =>
      choice(
        seq(field("name", $.identifier), "=", field("pattern", $.pattern)),
        field("name", $.identifier),
        "..",
      ),

    tuple_pattern: ($) =>
      seq("(", $.pattern, ",", commaSep1($.pattern), ")"),

    parenthesized_pattern: ($) => seq("(", $.pattern, ")"),

    // -- Expressions ---------------------------------------------------------

    expression: ($) =>
      choice(
        $.let_expression,
        $.if_expression,
        $.case_expression,
        $.match_expression,
        $.lambda_expression,
        $.loop_expression,
        $.do_expression,
        $.binary_expression,
        $.pipe_expression,
        $.dollar_expression,
        $._simple_expression,
      ),

    let_expression: ($) =>
      prec.right(-1, seq("let", repeat1($.let_binding), "in", $.expression)),

    let_binding: ($) =>
      seq(
        field("name", $.identifier),
        repeat($.pattern),
        "=",
        $.expression,
        optional($._layout_semicolon),
      ),

    if_expression: ($) =>
      prec.right(-1, seq("if", $.expression, "then", $.expression, "else", $.expression)),

    case_expression: ($) =>
      prec.right(-1, seq("case", $.expression, "of", sepBy1($._layout_semicolon, $.case_arm), optional($._layout_end))),

    match_expression: ($) =>
      prec.right(-1, seq("match", $.expression, sepBy1($._layout_semicolon, $.match_arm), optional($._layout_end))),

    case_arm: ($) =>
      prec.right(seq($.pattern, optional($.guard), "->", $.expression)),

    match_arm: ($) =>
      prec.right(seq("|", $.pattern, optional($.guard), "->", $.expression)),

    guard: ($) =>
      seq("when", $.expression),

    lambda_expression: ($) =>
      prec.right(-1, seq("\\", repeat1($.pattern), "->", $.expression)),

    loop_expression: ($) =>
      prec.right(-1, seq("loop", field("name", $.identifier), repeat($.loop_binding), "in", field("body", $.expression))),

    loop_binding: ($) =>
      seq("(", field("name", $.identifier), "=", field("init", $.expression), ")"),

    do_expression: ($) =>
      prec.right(-1, seq("do", $.expression)),

    binary_expression: ($) =>
      choice(
        ...[
          ["+", 6],
          ["-", 6],
          ["*", 7],
          ["/", 7],
          ["%", 7],
          ["==", 4],
          ["/=", 4],
          ["<", 4],
          [">", 4],
          ["<=", 4],
          [">=", 4],
          ["&&", 3],
          ["||", 2],
          ["::", 5],
          ["&", 8],
          ["^", 8],
          ["<<", 9],
          [">>", 9],
        ].map(([op, prec_val]) =>
          prec.left(
            /** @type {number} */ (prec_val),
            seq(
              field("left", $.expression),
              field("operator", /** @type {string} */ (op)),
              field("right", $.expression),
            ),
          ),
        ),
      ),

    pipe_expression: ($) =>
      prec.left(1, seq($.expression, "|>", $.expression)),

    dollar_expression: ($) =>
      prec.right(0, seq($.expression, "$", $.expression)),

    _simple_expression: ($) =>
      choice(
        $.function_application,
        $.unary_expression,
        $._atomic_expression,
      ),

    unary_expression: ($) =>
      prec(10, seq(field("operator", "~"), $._atomic_expression)),

    // Negative number literal: -42, -3.14 (not -x, which is binary subtraction)
    negative_literal: ($) =>
      token(
        seq("-", choice(
          /0[xX][0-9a-fA-F_]+[ui]?/,
          /0[oO][0-7_]+[ui]?/,
          /0[bB][01_]+[ui]?/,
          /[0-9][0-9_]*[ui]?/,
          /[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9_]+)?/,
        )),
      ),

    function_application: ($) =>
      prec.left(10, seq($._simple_expression, $._atomic_expression)),

    _atomic_expression: ($) =>
      choice(
        $.identifier_expression,
        $.constructor_expression,
        $._literal,
        $.negative_literal,
        $.field_access,
        $.index_expression,
        $.list_expression,
        $.record_expression,
        $.record_update,
        $.tuple_expression,
        $.unit_expression,
        $.parenthesized_expression,
        $.negation_expression,
        $.builtin_identifier,
      ),

    identifier_expression: ($) => $.identifier,
    constructor_expression: ($) => $.upper_identifier,

    field_access: ($) =>
      prec.left(11, seq($._simple_expression, ".", $.identifier)),

    index_expression: ($) =>
      prec.left(11, seq($._simple_expression, "[", $.expression, "]")),

    list_expression: ($) =>
      seq("[", commaSep($.expression), "]"),

    record_expression: ($) =>
      seq($.upper_identifier, "{", commaSep1Trailing($.field_init), "}"),

    record_update: ($) =>
      seq($._simple_expression, "{", commaSep1Trailing($.field_init), "}"),

    field_init: ($) =>
      seq(field("name", $.identifier), "=", field("value", $.expression)),

    tuple_expression: ($) =>
      seq("(", $.expression, ",", commaSep1($.expression), ")"),

    unit_expression: ($) => seq("(", ")"),

    parenthesized_expression: ($) =>
      seq("(", choice(
        // Negation in parens: (-x), (-b), (-(a + b))
        seq("-", $.expression),
        $.expression,
      ), ")"),

    // Prefix negation: -(expr). The token "-(" is matched as a single
    // immediate token so that binary subtraction (a - b) is never ambiguous.
    negation_expression: ($) =>
      seq(token(prec(1, "-(")), $.expression, ")"),

    // -- Identifiers and literals -------------------------------------------

    identifier: ($) => /[a-z_][a-zA-Z0-9_']*/,
    upper_identifier: ($) => /[A-Z][A-Za-z0-9_]*/,
    operator_name: ($) => seq("(", $.operator_symbol, ")"),
    operator_symbol: ($) => /[+\-*/%=<>!&^|~]+/,
    builtin_identifier: ($) => /\$[a-zA-Z_][a-zA-Z0-9_]*/,

    _literal: ($) =>
      choice(
        $.integer_literal,
        $.float_literal,
        $.string_literal,
        $.char_literal,
        $.boolean,
      ),

    integer_literal: ($) =>
      token(
        choice(
          /0[xX][0-9a-fA-F_]+[ui]?/,
          /0[oO][0-7_]+[ui]?/,
          /0[bB][01_]+[ui]?/,
          /[0-9][0-9_]*[ui]?/,
        ),
      ),

    float_literal: ($) =>
      token(/[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9_]+)?/),

    string_literal: ($) =>
      seq('"', repeat(choice(/[^"\\]+/, $.escape_sequence)), '"'),

    char_literal: ($) =>
      seq("'", choice(/[^'\\]/, $.escape_sequence), "'"),

    escape_sequence: ($) =>
      token.immediate(
        /\\(['"\\nrt0abfv]|x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\})/,
      ),

    boolean: ($) => choice("true", "false"),

    // -- Comments -----------------------------------------------------------

    line_comment: ($) => token(choice(seq("--", /.*/), seq("//", /.*/))),

    block_comment: ($) =>
      token(seq("{-", /[\s\S]*?/, "-}")),
  },
});

/**
 * Comma-separated list (0 or more).
 * @param {RuleOrLiteral} rule
 */
function commaSep(rule) {
  return optional(commaSep1(rule));
}

/**
 * Comma-separated list (1 or more).
 * @param {RuleOrLiteral} rule
 */
function commaSep1(rule) {
  return seq(rule, repeat(seq(",", rule)));
}

/**
 * One or more items separated by an optional separator.
 * Used for layout-separated items (e.g. let bindings separated by _layout_semicolon).
 * @param {RuleOrLiteral} sep
 * @param {RuleOrLiteral} rule
 */
function sepBy1(sep, rule) {
  return seq(rule, repeat(seq(optional(sep), rule)));
}

/**
 * Comma-separated list (1 or more) with optional trailing comma.
 * @param {RuleOrLiteral} rule
 */
function commaSep1Trailing(rule) {
  return seq(rule, repeat(seq(",", rule)), optional(","));
}

/**
 * Comma-separated list (0 or more) with optional trailing comma.
 * @param {RuleOrLiteral} rule
 */
function commaSepTrailing(rule) {
  return optional(commaSep1Trailing(rule));
}
