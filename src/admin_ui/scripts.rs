pub(super) fn admin_js() -> &'static str {
    concat!(
        "<script>\n",
        include_str!("../../assets/admin.js"),
        "</script>\n"
    )
}
