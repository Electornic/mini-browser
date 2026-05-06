#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mb_runtime::{
        chrome::{
            ADDRESS_BOX_HEIGHT, ADDRESS_BOX_X, ADDRESS_BOX_Y, BACK_BUTTON_X, CHROME_HEIGHT,
            ChromeAction, MENU_BUTTON_GAP, MENU_BUTTON_RIGHT_PAD, MENU_BUTTON_WIDTH, NAV_BUTTON_Y,
            address_bar_rect, back_button_rect, menu_button_rect, refresh_button_rect,
        },
        css,
        display_list::{
            LinkTarget, collect_image_commands, collect_link_targets, compute_hovered_dom_path,
            document_height, link_decoration_commands, point_in_rect,
        },
        html, layout,
        navigation::{describe_network_error, error_document, text_document},
        net, render, resource,
        state::{BrowserState, HistoryEntry, page_step},
        input, style,
    };

    #[test]
    fn computes_document_height_from_commands() {
        let commands = vec![
            render::DisplayCommand::SolidRect(
                css::Color::BLACK,
                layout::Rect {
                    x: 0.0,
                    y: 10.0,
                    width: 20.0,
                    height: 30.0,
                },
            ),
            render::DisplayCommand::Text(render::TextCommand {
                text: "hello".into(),
                x: 0.0,
                y: 60.0,
                color: css::Color::BLACK,
                font_size: 8.0,
                wrap_width: None,
                font_family: None,
            }),
        ];

        assert_eq!(document_height(&commands), 68.0);
    }

    #[test]
    fn page_step_uses_visible_height() {
        let expected = 400.0 - CHROME_HEIGHT - 24.0;
        assert_eq!(page_step(400), expected);
        assert_eq!(page_step(40), 24.0);
    }

    #[test]
    fn collects_link_targets_from_layout_tree() {
        let document = html::parse(r#"<a href="/next"><span>Hello</span></a>"#).unwrap();
        let node = document.roots()[0];
        let styled = style::style_tree(&document, node, &[]);
        let layout_tree = layout::layout_tree(&styled, 300.0);
        let links = collect_link_targets(&layout_tree, None, false, render::Affine::IDENTITY);

        assert_eq!(links.len(), 2);
        assert_eq!(links[0].href, "/next");
        assert_eq!(links[1].href, "/next");
        assert!(!links[0].underline);
        assert!(links[1].underline);
    }

    #[test]
    fn text_decoration_none_suppresses_link_underline() {
        let document = html::parse(r#"<a href="/next" class="tile">Hello</a>"#).unwrap();
        let node = document.roots()[0];
        let stylesheet = css::parse(".tile { text-decoration: none; }").unwrap();
        let styled = style::style_tree(&document, node, &[stylesheet]);
        let layout_tree = layout::layout_tree(&styled, 300.0);
        let links = collect_link_targets(&layout_tree, None, false, render::Affine::IDENTITY);

        // Both the <a> target and the inherited text target keep their click rects, but the
        // text-decoration declaration on the <a> suppresses the underline that would normally
        // appear on the descendant text node.
        assert!(!links.is_empty());
        assert!(
            links.iter().all(|link| !link.underline),
            "no underline should be emitted when text-decoration is none"
        );
    }

    #[test]
    fn hit_testing_checks_rect_bounds() {
        assert!(point_in_rect(
            10.0,
            20.0,
            layout::Rect {
                x: 5.0,
                y: 10.0,
                width: 20.0,
                height: 20.0,
            },
        ));
        assert!(!point_in_rect(
            30.1,
            20.0,
            layout::Rect {
                x: 5.0,
                y: 10.0,
                width: 20.0,
                height: 20.0,
            },
        ));
    }

    #[test]
    fn address_bar_rect_matches_chrome_layout() {
        let rect = address_bar_rect(800.0);
        assert_eq!(rect.x, ADDRESS_BOX_X);
        assert_eq!(rect.y, ADDRESS_BOX_Y);
        assert_eq!(rect.height, ADDRESS_BOX_HEIGHT);
        // Address bar reserves space for the menu button on the right edge.
        let expected_width = 800.0
            - ADDRESS_BOX_X
            - (MENU_BUTTON_RIGHT_PAD + MENU_BUTTON_WIDTH + MENU_BUTTON_GAP);
        assert_eq!(rect.width, expected_width);
    }

    #[test]
    fn collects_image_commands_from_layout_tree() {
        let document = html::parse(r#"<img src="/pixel.png" width="12" height="8" />"#).unwrap();
        let node = document.roots()[0];
        let styled = style::style_tree(&document, node, &[]);
        let layout_tree = layout::layout_tree(&styled, 300.0);
        let mut images = HashMap::new();
        images.insert(
            "http://example.com/pixel.png".into(),
            resource::LoadedImage {
                url: net::Url::parse("http://example.com/pixel.png").unwrap(),
                width: 1,
                height: 1,
                pixels: vec![0xFF0000],
            },
        );

        let commands = collect_image_commands(
            &layout_tree,
            Some(&net::Url::parse("http://example.com/index.html").unwrap()),
            &images,
        );

        assert_eq!(
            commands,
            vec![render::DisplayCommand::Image(render::ImageCommand {
                x: 0.0,
                y: 0.0,
                width: 12.0,
                height: 8.0,
                source_width: 1,
                source_height: 1,
                pixels: vec![0xFF0000],
            })]
        );
    }

    #[test]
    fn error_document_escapes_html() {
        let (html, _) = error_document("load failed", "<bad>", "http://a.com?q=<x>");
        assert!(html.contains("&lt;bad&gt;"));
        assert!(html.contains("&lt;x&gt;"));
    }

    #[test]
    fn network_error_messages_are_human_readable() {
        assert_eq!(
            describe_network_error(&net::NetworkError::MissingLocationHeader),
            "redirect missing location"
        );
        assert_eq!(
            describe_network_error(&net::NetworkError::UnexpectedContentType(
                "application/pdf".into()
            )),
            "unsupported content type application/pdf"
        );
    }

    #[test]
    fn text_document_escapes_plain_text() {
        let (html, _) = text_document("a < b", "http://example.com/file.txt");
        assert!(html.contains("a &lt; b"));
        assert!(html.contains("text document"));
    }

    #[test]
    fn link_decoration_underlines_text_targets_and_highlights_hover() {
        let links = vec![
            LinkTarget {
                href: "/a".into(),
                rect: layout::Rect {
                    x: 10.0,
                    y: 20.0,
                    width: 30.0,
                    height: 12.0,
                },
                underline: false,
            },
            LinkTarget {
                href: "/a".into(),
                rect: layout::Rect {
                    x: 12.0,
                    y: 20.0,
                    width: 28.0,
                    height: 12.0,
                },
                underline: true,
            },
        ];

        let commands = link_decoration_commands(&links, Some("/a"));
        assert_eq!(
            commands,
            vec![render::DisplayCommand::SolidRect(
                css::Color {
                    r: 180,
                    g: 60,
                    b: 140,
                    a: 255,
                },
                layout::Rect {
                    x: 12.0,
                    y: 31.0,
                    width: 28.0,
                    height: 1.0,
                },
            )]
        );
    }

    #[test]
    fn history_navigation_restores_previous_entries() {
        let mut browser = BrowserState::new(
            "http://first.test".into(),
            "<div>first</div>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "loaded",
        );

        browser.commit_navigation(HistoryEntry {
            address_input: "http://second.test".into(),
            document_html: "<div>second</div>".into(),
            stylesheet: String::new(),
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
            current_url: None,
            status_text: "loaded".into(),
            status_color: css::Color::BLACK,
        });

        browser.go_back();
        assert_eq!(browser.address_input, "http://first.test");
        assert_eq!(browser.document_html, "<div>first</div>");

        browser.go_forward();
        assert_eq!(browser.address_input, "http://second.test");
        assert_eq!(browser.document_html, "<div>second</div>");
    }

    #[test]
    fn back_button_hover_requires_history() {
        let mut browser = BrowserState::new(
            "http://first.test".into(),
            "<div>first</div>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "loaded",
        );

        let hover = browser.hovered_chrome_action(
            &input::WindowInput {
                mouse_position: Some((BACK_BUTTON_X + 2.0, NAV_BUTTON_Y + 2.0)),
                ..input::WindowInput::default()
            },
            800,
        );
        assert_eq!(hover, None);

        browser.back_stack.push(browser.snapshot());
        let hover = browser.hovered_chrome_action(
            &input::WindowInput {
                mouse_position: Some((back_button_rect().x + 2.0, back_button_rect().y + 2.0)),
                ..input::WindowInput::default()
            },
            800,
        );
        assert_eq!(hover, Some(ChromeAction::Back));
    }

    #[test]
    fn hovered_dom_path_picks_deepest_layout_box_under_mouse() {
        // Build a tiny tree where only one nested element exists; the hit-test should walk
        // down to it. <div id="root"><span class="leaf">x</span></div>
        let html_source = r#"<div id="root"><span class="leaf">hi</span></div>"#;
        let css_source = r#"
            #root { width: 100px; height: 80px; }
            .leaf { width: 40px; height: 20px; }
        "#;
        let document = html::parse(html_source).unwrap();
        let node = document.roots()[0];
        let stylesheet = css::parse(css_source).unwrap();
        let styled = style::style_tree(&document, node, &[stylesheet]);
        let layout = layout::layout_tree(&styled, 800.0);

        // Mouse coordinates: window-space pointer over the leaf, accounting for the
        // chrome strip we subtract inside compute_hovered_dom_path.
        let leaf_window_y = CHROME_HEIGHT + 5.0;
        let path = compute_hovered_dom_path(
            &input::WindowInput {
                mouse_position: Some((10.0, leaf_window_y)),
                ..input::WindowInput::default()
            },
            &layout,
            0.0,
        );

        // Layout root is #root, its first child is the .leaf span ([0]), and the span's
        // text "hi" is laid out as the next inline child ([0, 0]). The hit-test descends
        // to the deepest containing box, so the text node wins.
        assert_eq!(path, Some(vec![0, 0]));
    }

    #[test]
    fn hovered_dom_path_accounts_for_transform_translate() {
        // The leaf is shifted right by 50px via `transform: translate`. A pointer
        // at the leaf's *original* logical x should now MISS, while a pointer at
        // the post-translate screen x should HIT the leaf.
        let html_source = r#"<div id="root"><span class="leaf">hi</span></div>"#;
        let css_source = r#"
            #root { width: 200px; height: 80px; }
            .leaf { width: 40px; height: 20px; transform: translate(50px, 0); }
        "#;
        let document = html::parse(html_source).unwrap();
        let node = document.roots()[0];
        let stylesheet = css::parse(css_source).unwrap();
        let styled = style::style_tree(&document, node, &[stylesheet]);
        let layout = layout::layout_tree(&styled, 800.0);

        // Logical x = 10 (inside leaf's untransformed box) but cursor is in
        // *screen* space — after the leaf is translated by 50, screen x=10
        // no longer overlaps the leaf, only the root.
        let leaf_window_y = CHROME_HEIGHT + 5.0;
        let logical_path = compute_hovered_dom_path(
            &input::WindowInput {
                mouse_position: Some((10.0, leaf_window_y)),
                ..input::WindowInput::default()
            },
            &layout,
            0.0,
        );
        // Root still covers the area around (10, 5); leaf does not. The
        // hit-test should pick the root, not the now-shifted leaf.
        assert_eq!(logical_path, Some(vec![]));

        // Cursor at screen x=60 lands on the post-translate leaf box.
        let translated_path = compute_hovered_dom_path(
            &input::WindowInput {
                mouse_position: Some((60.0, leaf_window_y)),
                ..input::WindowInput::default()
            },
            &layout,
            0.0,
        );
        assert_eq!(translated_path, Some(vec![0, 0]));
    }

    #[test]
    fn hovered_dom_path_accounts_for_transform_scale() {
        // The leaf is scaled 2x around its centre. Its logical box is
        // 40x20 at (0, 0) inside the root's content area; after the scale
        // its visible box becomes 80x40 centered on (20, 10), so screen
        // x ∈ [-20, 60] and y ∈ [-10, 30] all hit it. (Only the part
        // overlapping the root will get hovered, since the deepest hit
        // wins.)
        let html_source = r#"<div id="root"><span class="leaf">hi</span></div>"#;
        let css_source = r#"
            #root { width: 200px; height: 80px; }
            .leaf { width: 40px; height: 20px; transform: scale(2); }
        "#;
        let document = html::parse(html_source).unwrap();
        let node = document.roots()[0];
        let stylesheet = css::parse(css_source).unwrap();
        let styled = style::style_tree(&document, node, &[stylesheet]);
        let layout = layout::layout_tree(&styled, 800.0);

        // Cursor at screen x=55: outside the leaf's *logical* 40-wide box,
        // but well inside the post-scale 80-wide visible box. Hit-test
        // should walk into the leaf (path [0]). The inner text glyph "hi"
        // does not extend to logical x=37.5, so we stop at the leaf and
        // not its text child.
        let leaf_window_y = CHROME_HEIGHT + 5.0;
        let path = compute_hovered_dom_path(
            &input::WindowInput {
                mouse_position: Some((55.0, leaf_window_y)),
                ..input::WindowInput::default()
            },
            &layout,
            0.0,
        );
        assert_eq!(path, Some(vec![0]));

        // Sanity: with no transform, screen x=55 lies *outside* the
        // unscaled 40-wide leaf, so hit-test returns the root path. This
        // confirms the leaf hit above is genuinely caused by the scale.
        let no_transform_html = r#"<div id="root"><span class="leaf">hi</span></div>"#;
        let no_transform_css = r#"
            #root { width: 200px; height: 80px; }
            .leaf { width: 40px; height: 20px; }
        "#;
        let plain_document = html::parse(no_transform_html).unwrap();
        let plain_node = plain_document.roots()[0];
        let plain_sheet = css::parse(no_transform_css).unwrap();
        let plain_styled = style::style_tree(&plain_document, plain_node, &[plain_sheet]);
        let plain_layout = layout::layout_tree(&plain_styled, 800.0);
        let plain_path = compute_hovered_dom_path(
            &input::WindowInput {
                mouse_position: Some((55.0, leaf_window_y)),
                ..input::WindowInput::default()
            },
            &plain_layout,
            0.0,
        );
        assert_eq!(plain_path, Some(vec![]));
    }

    #[test]
    fn hovered_dom_path_accounts_for_transform_rotate() {
        // Rotate a 20×20 square leaf 45° around its centre (10, 10). The
        // rotated diamond extends beyond the leaf's logical x range (out
        // to ~24) along the screen axis, so a cursor parked at screen
        // (23, 10) must hit the leaf even though the same cursor would
        // miss the unrotated 20×20 box.
        // Use a div with no text child so the deepest hit is unambiguously
        // the leaf — adding text would introduce an inline-flow child whose
        // own box also picks up the inherited rotation transform, and it is
        // separately interesting to track its post-transform extent.
        let html_source = r#"<div id="root"><div class="leaf"></div></div>"#;
        let css_source = r#"
            #root { width: 200px; height: 80px; }
            .leaf { width: 20px; height: 20px; transform: rotate(45deg); }
        "#;
        let document = html::parse(html_source).unwrap();
        let node = document.roots()[0];
        let stylesheet = css::parse(css_source).unwrap();
        let styled = style::style_tree(&document, node, &[stylesheet]);
        let layout = layout::layout_tree(&styled, 800.0);

        let leaf_window_y = CHROME_HEIGHT + 10.0;
        let path = compute_hovered_dom_path(
            &input::WindowInput {
                mouse_position: Some((23.0, leaf_window_y)),
                ..input::WindowInput::default()
            },
            &layout,
            0.0,
        );
        assert_eq!(path, Some(vec![0]));

        // Sanity: with no rotation, the same cursor lands outside the leaf
        // and the deepest hit is the root.
        let plain_html = r#"<div id="root"><div class="leaf"></div></div>"#;
        let plain_css = r#"
            #root { width: 200px; height: 80px; }
            .leaf { width: 20px; height: 20px; }
        "#;
        let plain_document = html::parse(plain_html).unwrap();
        let plain_node = plain_document.roots()[0];
        let plain_sheet = css::parse(plain_css).unwrap();
        let plain_styled = style::style_tree(&plain_document, plain_node, &[plain_sheet]);
        let plain_layout = layout::layout_tree(&plain_styled, 800.0);
        let plain_path = compute_hovered_dom_path(
            &input::WindowInput {
                mouse_position: Some((23.0, leaf_window_y)),
                ..input::WindowInput::default()
            },
            &plain_layout,
            0.0,
        );
        assert_eq!(plain_path, Some(vec![]));
    }

    #[test]
    fn hovered_dom_path_returns_none_when_pointer_is_in_chrome() {
        let html_source = r#"<div id="root"><span class="leaf">hi</span></div>"#;
        let document = html::parse(html_source).unwrap();
        let node = document.roots()[0];
        let styled = style::style_tree(&document, node, &[]);
        let layout = layout::layout_tree(&styled, 800.0);

        // Pointer parked above the chrome cutoff — there is no page element to hover.
        let path = compute_hovered_dom_path(
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT - 1.0)),
                ..input::WindowInput::default()
            },
            &layout,
            0.0,
        );
        assert_eq!(path, None);
    }

    #[test]
    fn refresh_button_is_hover_able_without_current_url() {
        let browser = BrowserState::new(
            String::new(),
            String::new(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "",
        );
        let refresh_rect = refresh_button_rect();
        let hover = browser.hovered_chrome_action(
            &input::WindowInput {
                mouse_position: Some((refresh_rect.x + 2.0, refresh_rect.y + 2.0)),
                ..input::WindowInput::default()
            },
            800,
        );
        assert_eq!(hover, Some(ChromeAction::Refresh));
    }

    #[test]
    fn refresh_without_current_url_sets_status_and_does_not_fetch() {
        // On the NTP there is no document to reload — the click should land cleanly with
        // a status hint rather than triggering an empty-URL network fetch.
        let mut browser = BrowserState::new(
            String::new(),
            "<div>ntp</div>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "",
        );
        let original_html = browser.document_html.clone();
        let refresh_rect = refresh_button_rect();

        browser.apply_input(
            &input::WindowInput {
                mouse_position: Some((refresh_rect.x + 2.0, refresh_rect.y + 2.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.status_text, "nothing to refresh");
        assert_eq!(browser.document_html, original_html);
    }

    #[test]
    fn menu_button_hover_is_independent_of_history() {
        let browser = BrowserState::new(
            "http://first.test".into(),
            "<div>first</div>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "loaded",
        );

        let menu_rect = menu_button_rect(800.0);
        let hover = browser.hovered_chrome_action(
            &input::WindowInput {
                mouse_position: Some((menu_rect.x + 2.0, menu_rect.y + 2.0)),
                ..input::WindowInput::default()
            },
            800,
        );
        assert_eq!(hover, Some(ChromeAction::Menu));
    }

    #[test]
    fn clicking_menu_button_sets_status_and_does_not_navigate() {
        let mut browser = BrowserState::new(
            "http://first.test".into(),
            "<div>first</div>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "loaded",
        );
        let original_html = browser.document_html.clone();
        let menu_rect = menu_button_rect(800.0);

        browser.apply_input(
            &input::WindowInput {
                mouse_position: Some((menu_rect.x + 2.0, menu_rect.y + 2.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        // Menu click registers as a chrome action: status flips to the stub label and the
        // current document is left untouched (no fall-through to page link handling).
        assert_eq!(browser.status_text, "menu (todo)");
        assert_eq!(browser.document_html, original_html);
    }

    fn browser_with_html(html: &str) -> BrowserState {
        BrowserState::new(
            "about:blank".into(),
            html.into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "",
        )
    }

    #[test]
    fn inline_script_runs_during_construction() {
        let mut browser = browser_with_html("<script>var phase2 = 42;</script>");
        assert_eq!(browser.js.execute("phase2").unwrap(), "42");
    }

    #[test]
    fn inline_scripts_execute_in_document_order() {
        let mut browser = browser_with_html(
            "<script>var n = 1;</script><div><script>n = n + 5;</script></div>",
        );
        assert_eq!(browser.js.execute("n").unwrap(), "6");
    }

    #[test]
    fn navigation_resets_js_runtime() {
        let mut browser = browser_with_html("<script>var leaked = 'first';</script>");
        assert_eq!(browser.js.execute("leaked").unwrap(), "\"first\"");
        // install_document funnels every navigation/back-forward; it must clear
        // page-defined globals so the next document starts clean.
        browser.install_document("<p>second</p>".into(), String::new(), HashMap::new());
        assert!(browser.js.execute("leaked").is_err());
    }

    fn browser_with_externals(html: &str, externals: HashMap<String, String>) -> BrowserState {
        BrowserState::new(
            "about:blank".into(),
            html.into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            externals,
            None,
            "",
        )
    }

    #[test]
    fn external_script_body_runs_when_present_in_externals_map() {
        let externals = HashMap::from([("lib.js".to_string(), "var lib = 7;".to_string())]);
        let mut browser = browser_with_externals(r#"<script src="lib.js"></script>"#, externals);
        assert_eq!(browser.js.execute("lib").unwrap(), "7");
    }

    #[test]
    fn external_script_with_missing_body_is_silently_skipped() {
        // Empty externals map simulates a fetch failure — `missing.js` simply has
        // no entry. The browser must not error; later inline scripts must still run.
        let mut browser = browser_with_externals(
            r#"<script src="missing.js"></script><script>var still_ran = 1;</script>"#,
            HashMap::new(),
        );
        assert_eq!(browser.js.execute("still_ran").unwrap(), "1");
    }

    #[test]
    fn inline_script_can_read_dom_via_document_get_element_by_id() {
        // Run a <script> that depends on `document.getElementById` resolving
        // against the page's parsed DOM. Confirms the browser-level wiring
        // (BrowserState::run_scripts → js.bind_document → js.execute) hands
        // the engine the document it just installed, not an empty tree.
        let mut browser = browser_with_html(
            r#"<div id="hero">welcome</div><script>var greeting = document.getElementById('hero').textContent;</script>"#,
        );
        assert_eq!(browser.js.execute("greeting").unwrap(), "\"welcome\"");
    }

    #[test]
    fn navigation_rebinds_dom_for_next_document() {
        // After install_document a new page, the same JS APIs must resolve
        // against the new DOM. Catches a regression where bind_document is
        // forgotten on the second-and-later install path.
        let mut browser = browser_with_html(r#"<p id="x">first</p>"#);
        assert_eq!(
            browser
                .js
                .execute("document.getElementById('x').textContent")
                .unwrap(),
            "\"first\""
        );
        browser.install_document(
            r#"<p id="y">second</p>"#.into(),
            String::new(),
            HashMap::new(),
        );
        assert_eq!(
            browser
                .js
                .execute("document.getElementById('x')")
                .unwrap(),
            "null"
        );
        assert_eq!(
            browser
                .js
                .execute("document.getElementById('y').textContent")
                .unwrap(),
            "\"second\""
        );
    }

    #[test]
    fn inline_and_external_scripts_execute_in_document_order() {
        // The order must be: inline 'a' → external 'b' → inline 'c'. If externals
        // were appended after all inlines (or vice versa), `seq` would not be "abc".
        let externals = HashMap::from([("b.js".to_string(), r#"seq += "b";"#.to_string())]);
        let mut browser = browser_with_externals(
            r#"<script>var seq = "a";</script><script src="b.js"></script><script>seq += "c";</script>"#,
            externals,
        );
        assert_eq!(browser.js.execute("seq").unwrap(), "\"abc\"");
    }

    #[test]
    fn inline_script_text_content_mutation_persists_in_browser_dom() {
        // Step 5.1 contract: BrowserState's `parsed_document` Rc and the
        // JsRuntime's Rc point at the same arena. A script that mutates the
        // DOM at install time must therefore be observable by BrowserState
        // post-construction — that's what feeds the next frame's layout.
        let browser = browser_with_html(
            r#"<div id="host"><div id="x"></div><script>document.getElementById('x').textContent='hi';</script></div>"#,
        );
        let document = browser.parsed_document.borrow();
        let host = document.roots()[0];
        let host_kids = &document.get(host).unwrap().children;
        // host has [div#x, script]; div#x is the first child after parsing.
        let target = host_kids[0];
        let target_kids = &document.get(target).unwrap().children;
        assert_eq!(target_kids.len(), 1);
        assert_eq!(document.text(target_kids[0]), Some("hi"));
    }

    #[test]
    fn inline_script_insert_before_persists_in_browser_dom() {
        // Step 5.2: insertBefore must reach the same arena as appendChild
        // does. Builds a list with two preexisting <li>s and threads a third
        // one between them at install time.
        let browser = browser_with_html(
            r#"<ul id="list"><li id="a">a</li><li id="c">c</li></ul><script>
                 var list = document.getElementById('list');
                 var c = document.getElementById('c');
                 var b = document.createElement('li');
                 b.textContent = 'b';
                 list.insertBefore(b, c);
               </script>"#,
        );
        let document = browser.parsed_document.borrow();
        let ul = document.roots()[0];
        let kids = &document.get(ul).unwrap().children;
        assert_eq!(kids.len(), 3);
        // Final order is a, b, c — so kids[1] is the freshly inserted <li>.
        let li_b = kids[1];
        let li_b_kids = &document.get(li_b).unwrap().children;
        assert_eq!(li_b_kids.len(), 1);
        assert_eq!(document.text(li_b_kids[0]), Some("b"));
    }

    #[test]
    fn inline_script_create_text_node_appended_persists_in_browser_dom() {
        // The other half of the Step 5.2 surface: createTextNode produces a
        // Text node whose NodeId is appendable into the same arena. Without
        // this, scripts wanting to insert plain text would have to resort to
        // textContent= replacement, which clobbers any pre-existing children.
        let browser = browser_with_html(
            r#"<p id="host">existing </p><script>
                 var host = document.getElementById('host');
                 host.appendChild(document.createTextNode('appended'));
               </script>"#,
        );
        let document = browser.parsed_document.borrow();
        let host = document.roots()[0];
        let kids = &document.get(host).unwrap().children;
        // Two text-node siblings: the parser-produced "existing " and the
        // script-appended "appended". The element survives intact.
        assert_eq!(kids.len(), 2);
        assert_eq!(document.text(kids[0]), Some("existing "));
        assert_eq!(document.text(kids[1]), Some("appended"));
    }

    #[test]
    fn inline_script_append_child_persists_in_browser_dom() {
        // Same arena-sharing contract, exercised through createElement +
        // appendChild + textContent setter chained together — a regression
        // here means the mutation API isn't really mutating BrowserState's
        // arena, even if it round-trips through the JS bridge alone.
        let browser = browser_with_html(
            r#"<ul id="list"></ul><script>
                 var list = document.getElementById('list');
                 var li = document.createElement('li');
                 li.textContent = 'one';
                 list.appendChild(li);
               </script>"#,
        );
        let document = browser.parsed_document.borrow();
        // <ul> is the first root; after the script, it should have exactly
        // one <li> child with text "one".
        let ul = document.roots()[0];
        let kids = &document.get(ul).unwrap().children;
        assert_eq!(kids.len(), 1);
        let li_kids = &document.get(kids[0]).unwrap().children;
        assert_eq!(li_kids.len(), 1);
        assert_eq!(document.text(li_kids[0]), Some("one"));
    }

    // ---- Step 6 events: page-area click → JS dispatch_event ----

    fn browser_with_html_and_css(html: &str, css: &str) -> BrowserState {
        BrowserState::new(
            "about:blank".into(),
            html.into(),
            css.into(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "",
        )
    }

    #[test]
    fn page_click_dispatches_click_event_to_target_element() {
        // The script registers a listener at install time; the layout
        // root is the trailing div (build_document_view picks
        // roots().last()), so a press at (50, CHROME+10) lands on it
        // and bubbles into the click handler.
        let mut browser = browser_with_html_and_css(
            r#"<script>var clicks = 0; document.getElementById('kid').addEventListener('click', function() { clicks = clicks + 1; });</script><div id="kid">x</div>"#,
            r#"#kid { width: 200px; height: 100px; }"#,
        );
        assert_eq!(browser.js.execute("clicks").unwrap(), "0");
        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((50.0, CHROME_HEIGHT + 10.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );
        assert_eq!(browser.js.execute("clicks").unwrap(), "1");
    }

    #[test]
    fn click_above_the_chrome_cutoff_does_not_fire_page_listeners() {
        // Clicks above CHROME_HEIGHT belong to the address bar / nav
        // buttons; the page-area dispatch must skip them so chrome
        // interactions don't double-fire as DOM clicks underneath.
        let mut browser = browser_with_html_and_css(
            r#"<script>var clicks = 0; document.getElementById('kid').addEventListener('click', function() { clicks = clicks + 1; });</script><div id="kid">x</div>"#,
            r#"#kid { width: 200px; height: 100px; }"#,
        );
        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((50.0, CHROME_HEIGHT - 1.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );
        assert_eq!(browser.js.execute("clicks").unwrap(), "0");
    }

    #[test]
    fn page_click_bubbles_into_ancestor_listener_via_browser_pipeline() {
        // End-to-end: layout box for `inner` is the deepest hit, dispatch
        // promotes the bubble up to outer. Confirms compute_hovered_hit
        // and dispatch_event line up with each other when funnelled
        // through display_list.
        let mut browser = browser_with_html_and_css(
            r#"<script>var trace = ''; document.getElementById('outer').addEventListener('click', function() { trace += 'outer:'; }); document.getElementById('inner').addEventListener('click', function() { trace += 'inner:'; });</script><div id="outer"><div id="inner">x</div></div>"#,
            r#"#outer { width: 200px; height: 100px; } #inner { width: 100px; height: 50px; }"#,
        );
        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT + 5.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );
        assert_eq!(
            browser.js.execute("trace").unwrap(),
            "\"inner:outer:\""
        );
    }

    #[test]
    fn click_handler_dom_mutation_is_visible_to_browser_state() {
        // Step 6 keeps the same Rc<RefCell<Document>> sharing contract
        // Step 5.x set up: a handler that calls appendChild during dispatch
        // mutates the same arena BrowserState reads back here.
        let mut browser = browser_with_html_and_css(
            r#"<script>document.getElementById('host').addEventListener('click', function() { var p = document.createElement('p'); p.textContent = 'inserted'; document.getElementById('host').appendChild(p); });</script><div id="host"></div>"#,
            r#"#host { width: 200px; height: 100px; }"#,
        );
        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT + 5.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );
        let document = browser.parsed_document.borrow();
        // Two roots: the leading <script> and the trailing <div id=host>.
        // The host is the second one — the handler should have appended
        // a single <p> child.
        let host = *document.roots().last().unwrap();
        let host_kids = &document.get(host).unwrap().children;
        assert_eq!(host_kids.len(), 1);
        let p_kids = &document.get(host_kids[0]).unwrap().children;
        assert_eq!(p_kids.len(), 1);
        assert_eq!(document.text(p_kids[0]), Some("inserted"));
    }

    #[test]
    fn click_on_link_with_prevent_default_skips_navigation() {
        // The first "JS suppresses a default browser action" path — a
        // handler on the click bubble calls e.preventDefault(), and
        // BrowserState's display_list must skip the link follow-through.
        // Without the suppress path, navigate_to_link tries to resolve
        // "/next" against the (None) current_url, fails, and shows an
        // error page; document_html would change. With preventDefault
        // honoured, the original document survives the click intact.
        let mut browser = browser_with_html_and_css(
            r#"<script>document.getElementById('outer').addEventListener('click', function(e) { e.preventDefault(); });</script><div id="outer"><a id="lnk" href="/next">go</a></div>"#,
            r#"#outer { width: 200px; height: 100px; }"#,
        );
        let original_html = browser.document_html.clone();
        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT + 5.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );
        assert_eq!(browser.document_html, original_html);
    }

    #[test]
    fn page_click_fires_focus_on_target_and_blur_on_previously_focused_element() {
        // First click: a fresh focus lands on `a`. Second click on `b`:
        // blur fires on `a` first (the previously-focused path), then
        // focus on `b`. Tracing the order pins the contract end-to-end:
        // focused_dom_path tracking + non-bubbling dispatch + ordering.
        let mut browser = browser_with_html_and_css(
            concat!(
                "<script>",
                "var trace = '';",
                "document.getElementById('a').addEventListener('focus', function() { trace += 'a-focus;'; });",
                "document.getElementById('a').addEventListener('blur',  function() { trace += 'a-blur;'; });",
                "document.getElementById('b').addEventListener('focus', function() { trace += 'b-focus;'; });",
                "document.getElementById('b').addEventListener('blur',  function() { trace += 'b-blur;'; });",
                "</script>",
                r#"<div><div id="a">a</div><div id="b">b</div></div>"#,
            ),
            r#"#a { width: 200px; height: 50px; } #b { width: 200px; height: 50px; }"#,
        );
        // First press over the `a` box: a single focus, no blur (nothing was focused yet).
        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT + 10.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );
        assert_eq!(browser.js.execute("trace").unwrap(), "\"a-focus;\"");
        // Second press over the `b` box: blur on `a` then focus on `b`.
        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT + 60.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );
        assert_eq!(
            browser.js.execute("trace").unwrap(),
            "\"a-focus;a-blur;b-focus;\""
        );
    }

    #[test]
    fn click_above_chrome_clears_focus_and_fires_blur_on_previously_focused() {
        // Mouse press over the chrome (y < CHROME_HEIGHT) sets the new
        // focus to None; the previously-focused element should still
        // see a blur, but no focus event fires (no new target).
        let mut browser = browser_with_html_and_css(
            concat!(
                "<script>",
                "var trace = '';",
                "document.getElementById('x').addEventListener('focus', function() { trace += 'focus;'; });",
                "document.getElementById('x').addEventListener('blur',  function() { trace += 'blur;'; });",
                "</script>",
                r#"<div id="x">x</div>"#,
            ),
            r#"#x { width: 200px; height: 100px; }"#,
        );
        // Step 1: focus the page element.
        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT + 10.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );
        assert_eq!(browser.js.execute("trace").unwrap(), "\"focus;\"");
        // Step 2: click in the chrome band — focus clears, only blur fires.
        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT - 1.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );
        assert_eq!(browser.js.execute("trace").unwrap(), "\"focus;blur;\"");
    }

    // ---- Step 7 async: timers + rAF pumped per frame by display_list ----

    #[test]
    fn set_timeout_zero_fires_during_next_display_list_frame() {
        // setTimeout(fn, 0) lands with a now-aligned deadline (StdClock).
        // The first display_list call after install_document drains the
        // job queue at the top of the frame, firing the handler before
        // any layout happens.
        let mut browser = browser_with_html_and_css(
            r#"<script>var hits = 0; setTimeout(function () { hits = hits + 1; }, 0);</script>"#,
            "",
        );
        // The script's own microtask drain inside execute already fires
        // the timer (deadline == now), so the page already saw the tick.
        // Confirm here, then verify the next frame doesn't double-fire.
        assert_eq!(browser.js.execute("hits").unwrap(), "1");
        let _ = browser.display_list(800, 600, &input::WindowInput::default());
        assert_eq!(browser.js.execute("hits").unwrap(), "1");
    }

    #[test]
    fn request_animation_frame_callback_runs_during_next_display_list_frame() {
        // requestAnimationFrame doesn't fire inside `execute` — the
        // callback queues for the per-frame `run_animation_frame_callbacks`
        // call. The first display_list after install drains it.
        let mut browser = browser_with_html_and_css(
            r#"<script>var hits = 0; requestAnimationFrame(function () { hits = hits + 1; });</script>"#,
            "",
        );
        assert_eq!(browser.js.execute("hits").unwrap(), "0");
        let _ = browser.display_list(800, 600, &input::WindowInput::default());
        assert_eq!(browser.js.execute("hits").unwrap(), "1");
        // Snapshot-then-fire: re-rAF inside the handler would queue for
        // next frame, but this handler doesn't, so a second frame is a
        // no-op.
        let _ = browser.display_list(800, 600, &input::WindowInput::default());
        assert_eq!(browser.js.execute("hits").unwrap(), "1");
    }

    // ---- Step 6 (#6 in Notion): page input keyboard typing ----

    #[test]
    fn typed_chars_append_to_focused_input_value_attribute() {
        // <input> wrapped in a div so the input sits at DOM path [0].
        // Setting focused_dom_path directly skips the click+hit-test
        // dance — the typing logic itself is what's under test.
        let mut browser = browser_with_html(r#"<div><input value="ab"/></div>"#);
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![0]);

        browser.apply_input(
            &input::WindowInput {
                typed: "cd".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        let document = browser.parsed_document.borrow();
        let input_id = document.get(document.roots()[0]).unwrap().children[0];
        let elem = document.element_data(input_id).unwrap();
        assert_eq!(elem.attributes.get("value").map(String::as_str), Some("abcd"));
    }

    #[test]
    fn backspace_pops_last_char_from_focused_input() {
        let mut browser = browser_with_html(r#"<div><input value="hello"/></div>"#);
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![0]);

        browser.apply_input(
            &input::WindowInput {
                backspace_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        let document = browser.parsed_document.borrow();
        let input_id = document.get(document.roots()[0]).unwrap().children[0];
        let elem = document.element_data(input_id).unwrap();
        assert_eq!(elem.attributes.get("value").map(String::as_str), Some("hell"));
    }

    #[test]
    fn typing_with_no_focus_does_not_mutate_dom() {
        // Nothing focused → keystrokes are silently dropped. Input attribute
        // stays exactly as parsed; no panic on the missing focus path.
        let mut browser = browser_with_html(r#"<input value="frozen"/>"#);
        browser.address_bar_focused = false;
        browser.focused_dom_path = None;

        browser.apply_input(
            &input::WindowInput {
                typed: "abc".into(),
                backspace_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        let document = browser.parsed_document.borrow();
        let input_id = document.roots()[0];
        let elem = document.element_data(input_id).unwrap();
        assert_eq!(
            elem.attributes.get("value").map(String::as_str),
            Some("frozen")
        );
    }

    #[test]
    fn typing_into_focused_non_input_element_is_no_op() {
        // Focus on a <div> (e.g. a future tabbable surface) — typing must
        // not corrupt arbitrary attributes on it. The protective tag check
        // inside `dispatch_typed_keys`'s default-action helpers is what
        // locks this contract.
        let mut browser = browser_with_html(r#"<div id="host"></div>"#);
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                typed: "x".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        let document = browser.parsed_document.borrow();
        let div_id = document.roots()[0];
        let elem = document.element_data(div_id).unwrap();
        // Only `id` should be present — no rogue `value` attribute appeared.
        assert_eq!(elem.attributes.get("value"), None);
    }

    #[test]
    fn address_bar_focus_takes_priority_over_page_input() {
        // Both flags set → address bar wins (matches the order in
        // apply_input). This is what makes Cmd+L "rescue" navigation
        // even when a page input had keyboard focus.
        let mut browser = browser_with_html(r#"<div><input value=""/></div>"#);
        browser.address_bar_focused = true;
        browser.address_bar_selected = false;
        browser.focused_dom_path = Some(vec![0]);
        // Reset the seeded "about:blank" address so we can pin the typed
        // suffix exactly. Real users would already be typing into a fresh
        // bar after Cmd+L (which sets selected=true and clears on first key).
        browser.address_input.clear();

        browser.apply_input(
            &input::WindowInput {
                typed: "y".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        // Address bar absorbed the keystroke …
        assert_eq!(browser.address_input, "y");
        // … the page input value attribute stayed empty.
        let document = browser.parsed_document.borrow();
        let input_id = document.get(document.roots()[0]).unwrap().children[0];
        let elem = document.element_data(input_id).unwrap();
        assert_eq!(
            elem.attributes.get("value").map(String::as_str),
            Some("")
        );
    }

    // ---- Step 6 integration: typing ↔ JS round-trip ----

    #[test]
    fn page_input_keyboard_typing_is_visible_to_javascript() {
        // Closes the Step 6 loop: a real keyboard event lands in the
        // `value` attribute (Step 6.3), and a JS read via the new
        // `.value` accessor (Step 6.4) sees the up-to-date string. This
        // is what would make a `oninput` handler (still TODO in #9)
        // observe the same text the user just typed.
        let mut browser = browser_with_html(r#"<input id="q" value="hi"/>"#);
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                typed: " world".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(
            browser
                .js
                .execute("document.getElementById('q').value")
                .unwrap(),
            "\"hi world\""
        );
    }

    #[test]
    fn js_set_value_is_visible_to_browser_state_layout_pass() {
        // The reverse direction: JS-driven `.value =` lands in the same
        // arena BrowserState reads on the next display_list frame, so the
        // input re-paints with the new text without any explicit
        // invalidation hop. Mirrors how class_list_mutations land.
        let mut browser = browser_with_html(r#"<input id="q" value="initial"/>"#);
        browser
            .js
            .execute("document.getElementById('q').value = 'changed';")
            .unwrap();

        let document = browser.parsed_document.borrow();
        let input_id = document.roots()[0];
        let elem = document.element_data(input_id).unwrap();
        assert_eq!(
            elem.attributes.get("value").map(String::as_str),
            Some("changed")
        );
    }

    // ---- Step 5 (#5 in Notion): keydown / keyup dispatch ----

    #[test]
    fn typing_fires_keydown_then_keyup_on_focused_input() {
        // A typed character should fire `keydown` first, then run the
        // default text-insert action, then fire `keyup` — the same
        // ordering real browsers expose. Tracing the order pins both
        // events as actually wired in.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var trace = '';",
            "document.getElementById('q').addEventListener('keydown', function(e) { trace += 'down:' + e.key + ';'; });",
            "document.getElementById('q').addEventListener('keyup',   function(e) { trace += 'up:'   + e.key + ';'; });",
            "</script>",
            r#"<input id="q"/>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                typed: "x".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(
            browser.js.execute("trace").unwrap(),
            "\"down:x;up:x;\""
        );
    }

    #[test]
    fn backspace_dispatches_keydown_with_backspace_key() {
        // event.key for the Backspace key must be the spec string
        // "Backspace" — handlers commonly switch on it.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var seen = '';",
            "document.getElementById('q').addEventListener('keydown', function(e) { seen = e.key; });",
            "</script>",
            r#"<input id="q" value="ab"/>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                backspace_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("seen").unwrap(), "\"Backspace\"");
    }

    #[test]
    fn enter_dispatches_keydown_with_enter_key_even_without_default_action() {
        // Enter has no default action yet (#7 will own form-submit), but
        // the keydown/keyup events must still fire so listeners can react.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var seen = '';",
            "document.getElementById('q').addEventListener('keydown', function(e) { seen = e.key; });",
            "</script>",
            r#"<input id="q"/>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                enter_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("seen").unwrap(), "\"Enter\"");
    }

    #[test]
    fn keydown_prevent_default_suppresses_typing_into_value() {
        // A `keydown` handler that calls `preventDefault()` must block
        // the default text-insert action — the input's `value`
        // attribute stays untouched, mirroring how preventDefault on a
        // link click suppresses navigation.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "document.getElementById('q').addEventListener('keydown', function(e) { e.preventDefault(); });",
            "</script>",
            r#"<input id="q" value=""/>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                typed: "abc".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        // The script root is roots()[0]; the input is roots()[1].
        let document = browser.parsed_document.borrow();
        let input_id = document.roots()[1];
        let elem = document.element_data(input_id).unwrap();
        assert_eq!(
            elem.attributes.get("value").map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn keydown_prevent_default_suppresses_backspace_pop() {
        // Same suppression contract on the Backspace path: preventDefault
        // on `keydown` blocks the value-pop default action.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "document.getElementById('q').addEventListener('keydown', function(e) { e.preventDefault(); });",
            "</script>",
            r#"<input id="q" value="hello"/>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                backspace_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        // The script root is roots()[0]; the input is roots()[1].
        let document = browser.parsed_document.borrow();
        let input_id = document.roots()[1];
        let elem = document.element_data(input_id).unwrap();
        assert_eq!(
            elem.attributes.get("value").map(String::as_str),
            Some("hello")
        );
    }

    #[test]
    fn keydown_bubbles_to_ancestor_listener() {
        // Same bubble contract `click` proved out: a `keydown` on the
        // focused <input> walks up to the surrounding <div>'s listener.
        let mut browser = browser_with_html_and_css(
            concat!(
                "<script>",
                "var trace = '';",
                "document.getElementById('outer').addEventListener('keydown', function() { trace += 'outer;'; });",
                "document.getElementById('inner').addEventListener('keydown', function() { trace += 'inner;'; });",
                "</script>",
                r#"<div id="outer"><input id="inner"/></div>"#,
            ),
            "",
        );
        browser.address_bar_focused = false;
        // last root is the <div>, whose first child (index 0) is the input.
        browser.focused_dom_path = Some(vec![0]);

        browser.apply_input(
            &input::WindowInput {
                typed: "x".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(
            browser.js.execute("trace").unwrap(),
            "\"inner;outer;\""
        );
    }

    #[test]
    fn keydown_event_target_is_the_focused_element() {
        // The Event object's `target` must point at the focused input,
        // even when the handler that reads it lives on an ancestor.
        // `tagName` (uppercase per the HTML spec) is the only Element
        // accessor reliably exposed today, so we read that to identify
        // the target — `INPUT` proves the bubble-aware retarget worked.
        let mut browser = browser_with_html_and_css(
            concat!(
                "<script>",
                "var seen = '';",
                "document.getElementById('outer').addEventListener('keydown', function(e) { seen = e.target.tagName; });",
                "</script>",
                r#"<div id="outer"><input id="inner"/></div>"#,
            ),
            "",
        );
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![0]);

        browser.apply_input(
            &input::WindowInput {
                typed: "y".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("seen").unwrap(), "\"INPUT\"");
    }

    #[test]
    fn address_bar_focus_suppresses_page_keydown_dispatch() {
        // When the address bar owns keyboard focus (Cmd+L state) the
        // page-side keydown path must not fire — typing rescues
        // navigation, it doesn't double-deliver to a stale page input.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var hits = 0;",
            "document.getElementById('q').addEventListener('keydown', function() { hits = hits + 1; });",
            "</script>",
            r#"<input id="q"/>"#,
        ));
        browser.address_bar_focused = true;
        browser.address_bar_selected = false;
        browser.focused_dom_path = Some(vec![]);
        browser.address_input.clear();

        browser.apply_input(
            &input::WindowInput {
                typed: "z".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        // Address bar absorbed the keystroke …
        assert_eq!(browser.address_input, "z");
        // … the page-side keydown listener saw nothing.
        assert_eq!(browser.js.execute("hits").unwrap(), "0");
    }

    #[test]
    fn keydown_fires_on_focused_non_input_without_value_mutation() {
        // Focus on a non-input element (a future tabbable surface):
        // keyboard events must still dispatch so handlers can react,
        // but the default text-insert is gated by the input tag check
        // and must not invent a `value` attribute on a `<div>`.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var hits = 0;",
            "document.getElementById('host').addEventListener('keydown', function() { hits = hits + 1; });",
            "</script>",
            r#"<div id="host"></div>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                typed: "k".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("hits").unwrap(), "1");
        // The script root is roots()[0]; the host div is roots()[1].
        let document = browser.parsed_document.borrow();
        let div_id = document.roots()[1];
        let elem = document.element_data(div_id).unwrap();
        assert_eq!(elem.attributes.get("value"), None);
    }

    // ---- Step 5b (#9 in Notion): input / change event dispatch ----

    #[test]
    fn typing_into_focused_input_fires_input_event_with_updated_value() {
        // The `input` event fires after the value is updated, so a
        // handler reading `event.target.value` sees the new string,
        // not the pre-keydown string. That's the contract real
        // frameworks build on (controlled-input bindings, etc.).
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var trace = '';",
            "document.getElementById('q').addEventListener('input', function(e) { trace += 'in:' + e.target.value + ';'; });",
            "</script>",
            r#"<input id="q" value="ab"/>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                typed: "c".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("trace").unwrap(), "\"in:abc;\"");
    }

    #[test]
    fn backspace_on_non_empty_value_fires_input_event() {
        // Successful pop is "real input" too — fires the event.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var hits = 0;",
            "document.getElementById('q').addEventListener('input', function() { hits = hits + 1; });",
            "</script>",
            r#"<input id="q" value="x"/>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                backspace_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("hits").unwrap(), "1");
    }

    #[test]
    fn backspace_on_empty_value_does_not_fire_input_event() {
        // Pop on an already-empty value isn't a value change, so the
        // `input` event must stay silent — matches real-browser
        // semantics where `input` fires only on actual changes.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var hits = 0;",
            "document.getElementById('q').addEventListener('input', function() { hits = hits + 1; });",
            "</script>",
            r#"<input id="q" value=""/>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                backspace_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("hits").unwrap(), "0");
    }

    #[test]
    fn keydown_prevent_default_also_suppresses_input_event() {
        // No value mutation → no `input`. The same preventDefault
        // path that gates the value-edit also gates the event.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var hits = 0;",
            "document.getElementById('q').addEventListener('keydown', function(e) { e.preventDefault(); });",
            "document.getElementById('q').addEventListener('input',   function() { hits = hits + 1; });",
            "</script>",
            r#"<input id="q"/>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                typed: "abc".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("hits").unwrap(), "0");
    }

    #[test]
    fn js_value_assignment_does_not_fire_input_event() {
        // Per the HTML spec, programmatic `.value = ...` does not fire
        // `input` — only user-driven changes do. This is the contract
        // that lets frameworks set the value during a render without
        // re-entering their own change handler.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var hits = 0;",
            "document.getElementById('q').addEventListener('input', function() { hits = hits + 1; });",
            "</script>",
            r#"<input id="q"/>"#,
        ));

        browser
            .js
            .execute("document.getElementById('q').value = 'set';")
            .unwrap();

        assert_eq!(browser.js.execute("hits").unwrap(), "0");
    }

    #[test]
    fn change_event_fires_on_blur_after_user_typed() {
        // End-to-end: type into a focused input, then click outside the
        // page. `change` fires before `blur` and bubbles per the modern
        // spec — both contracts pinned by trace ordering.
        let mut browser = browser_with_html_and_css(
            concat!(
                "<script>",
                "var trace = '';",
                "document.getElementById('q').addEventListener('change', function() { trace += 'change;'; });",
                "document.getElementById('q').addEventListener('blur',   function() { trace += 'blur;'; });",
                "</script>",
                r#"<input id="q"/>"#,
            ),
            "",
        );
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        // Type a character — sets the dirty flag.
        browser.apply_input(
            &input::WindowInput {
                typed: "a".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        // Click in the chrome band: new_focus = None, focus changes,
        // change-then-blur fires on the previously-focused input.
        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT - 1.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );

        assert_eq!(browser.js.execute("trace").unwrap(), "\"change;blur;\"");
    }

    #[test]
    fn change_event_silent_when_no_user_edit_during_focus() {
        // Same focus session as the previous test — but no typing, so
        // the dirty flag is never set and `change` must stay silent
        // even though `blur` fires on the focus move.
        let mut browser = browser_with_html_and_css(
            concat!(
                "<script>",
                "var trace = '';",
                "document.getElementById('q').addEventListener('change', function() { trace += 'change;'; });",
                "document.getElementById('q').addEventListener('blur',   function() { trace += 'blur;'; });",
                "</script>",
                r#"<input id="q"/>"#,
            ),
            "",
        );
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT - 1.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );

        assert_eq!(browser.js.execute("trace").unwrap(), "\"blur;\"");
    }

    #[test]
    fn js_value_assignment_does_not_dirty_focused_input_for_change() {
        // The focused input's value is set programmatically — that
        // must not arm the dirty flag, so the subsequent blur fires
        // alone with no `change` event.
        let mut browser = browser_with_html_and_css(
            concat!(
                "<script>",
                "var trace = '';",
                "document.getElementById('q').addEventListener('change', function() { trace += 'change;'; });",
                "document.getElementById('q').addEventListener('blur',   function() { trace += 'blur;'; });",
                "</script>",
                r#"<input id="q"/>"#,
            ),
            "",
        );
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser
            .js
            .execute("document.getElementById('q').value = 'set';")
            .unwrap();

        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT - 1.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );

        assert_eq!(browser.js.execute("trace").unwrap(), "\"blur;\"");
    }

    // ---- Step 5c (#7 in Notion): <button> + <form> submit ----

    #[test]
    fn enter_in_input_inside_form_dispatches_submit_event_on_the_form() {
        // Pressing Enter on a focused input that lives inside a form
        // is the classic "submit by keyboard" path. The submit handler
        // calls preventDefault so the browser doesn't try to navigate
        // (the action is relative + no current_url, which would land
        // on the error page and disturb subsequent assertions).
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var hits = 0;",
            "document.getElementById('f').addEventListener('submit', function(e) { hits = hits + 1; e.preventDefault(); });",
            "</script>",
            r#"<form id="f" action="/search"><input id="q" name="q" value="hi"/></form>"#,
        ));
        browser.address_bar_focused = false;
        // last root = form, child 0 = input
        browser.focused_dom_path = Some(vec![0]);

        browser.apply_input(
            &input::WindowInput {
                enter_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("hits").unwrap(), "1");
    }

    #[test]
    fn submit_event_target_is_the_form_not_the_input() {
        // The Event object's `target` must point at the <form>, not
        // at the input that triggered the submit. Real-world handlers
        // read e.target to read the form's attributes (action,
        // method) — this is the contract that lets them work.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var seen = '';",
            "document.getElementById('f').addEventListener('submit', function(e) { seen = e.target.tagName; e.preventDefault(); });",
            "</script>",
            r#"<form id="f"><input name="q"/></form>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![0]);

        browser.apply_input(
            &input::WindowInput {
                enter_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("seen").unwrap(), "\"FORM\"");
    }

    #[test]
    fn enter_in_input_outside_form_does_not_attempt_navigation() {
        // No enclosing form → Enter is just keydown/keyup, no submit
        // is dispatched, no navigation attempted. The document HTML
        // stays exactly as parsed at construction time.
        let mut browser = browser_with_html(r#"<input id="q" value="hi"/>"#);
        let original_html = browser.document_html.clone();
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                enter_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        // No form means no navigation: the document stayed the same.
        assert_eq!(browser.document_html, original_html);
    }

    #[test]
    fn submit_prevent_default_blocks_form_navigation() {
        // The "JS suppresses a default browser action" path, applied
        // to form submit: the handler calls preventDefault and the
        // browser must not navigate. Without this, a relative action
        // + no current_url would replace the document with an error
        // page, mutating `document_html`.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "document.getElementById('f').addEventListener('submit', function(e) { e.preventDefault(); });",
            "</script>",
            r#"<form id="f" action="/search"><input id="q" name="q" value="hi"/></form>"#,
        ));
        let original_html = browser.document_html.clone();
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![0]);

        browser.apply_input(
            &input::WindowInput {
                enter_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.document_html, original_html);
    }

    #[test]
    fn keydown_prevent_default_on_enter_blocks_form_submit_dispatch() {
        // The earlier preventDefault gate (on `keydown`) must also
        // suppress the form submit — otherwise the same key press
        // would still trigger the form even though the script said
        // "don't act on this key".
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var trace = '';",
            "document.getElementById('q').addEventListener('keydown', function(e) { e.preventDefault(); });",
            "document.getElementById('f').addEventListener('submit',  function() { trace += 'submit;'; });",
            "</script>",
            r#"<form id="f"><input id="q" name="q"/></form>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![0]);

        browser.apply_input(
            &input::WindowInput {
                enter_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("trace").unwrap(), "\"\"");
    }

    #[test]
    fn button_click_inside_form_dispatches_submit() {
        // A `<button>` with no explicit type defaults to type="submit"
        // per the HTML spec. Clicking it inside a form must fire the
        // submit event on that form. preventDefault keeps the test
        // off the navigation path.
        let mut browser = browser_with_html_and_css(
            concat!(
                "<script>",
                "var hits = 0;",
                "document.getElementById('f').addEventListener('submit', function(e) { hits = hits + 1; e.preventDefault(); });",
                "</script>",
                r#"<form id="f" action="/go"><button id="b">Submit</button></form>"#,
            ),
            r#"#b { display: block; width: 200px; height: 50px; }"#,
        );

        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT + 10.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );

        assert_eq!(browser.js.execute("hits").unwrap(), "1");
    }

    #[test]
    fn button_with_explicit_type_button_does_not_submit() {
        // type="button" opts out of the default-submit behaviour.
        // Clicking it must NOT fire the submit event on the form,
        // matching real browsers — `<button type="button">` is the
        // standard "non-submitting button" used for plain JS hooks.
        let mut browser = browser_with_html_and_css(
            concat!(
                "<script>",
                "var hits = 0;",
                "document.getElementById('f').addEventListener('submit', function() { hits = hits + 1; });",
                "</script>",
                r#"<form id="f"><button id="b" type="button">Click</button></form>"#,
            ),
            r#"#b { display: block; width: 200px; height: 50px; }"#,
        );

        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT + 10.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );

        assert_eq!(browser.js.execute("hits").unwrap(), "0");
    }

    #[test]
    fn button_outside_any_form_does_not_dispatch_submit() {
        // A bare `<button>` with no enclosing form has nothing to
        // submit; the click runs through dispatch_event for the click
        // event itself (handled elsewhere) but the form-submit lookup
        // returns None and does nothing.
        let mut browser = browser_with_html_and_css(
            concat!(
                "<script>",
                "var trace = '';",
                "document.getElementById('b').addEventListener('click',  function() { trace += 'click;'; });",
                "document.getElementById('b').addEventListener('submit', function() { trace += 'submit;'; });",
                "</script>",
                r#"<button id="b">Lonely</button>"#,
            ),
            r#"#b { display: block; width: 200px; height: 50px; }"#,
        );

        let _ = browser.display_list(
            800,
            600,
            &input::WindowInput {
                mouse_position: Some((10.0, CHROME_HEIGHT + 10.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
        );

        // Click fired (proves the button was hit), submit didn't.
        assert_eq!(browser.js.execute("trace").unwrap(), "\"click;\"");
    }

    // ---- Step 8 (#8 in Notion): <textarea> multi-line text field ----
    //
    // <textarea> reuses the entire <input> typing path: the same
    // dispatch_typed_keys handler routes typed chars into the same
    // `value` attribute. The two divergences live behind tag checks:
    //   * Enter inserts `\n` into the value instead of submitting the
    //     enclosing form.
    //   * Form data collection picks up textareas alongside inputs so
    //     a multi-line message lands in the GET query string.

    #[test]
    fn typing_into_focused_textarea_appends_to_value_attribute() {
        // Smoke check that the Step 6/Step 9 typing pipeline routes
        // characters into <textarea> the same way it does into <input>.
        let mut browser = browser_with_html(r#"<textarea id="t" value="hi"></textarea>"#);
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                typed: "!".into(),
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(
            browser.js.execute("document.getElementById('t').value").unwrap(),
            "\"hi!\""
        );
    }

    #[test]
    fn enter_in_focused_textarea_inserts_newline_instead_of_submitting() {
        // Inside a <form>, an <input> Enter triggers a submit. A
        // <textarea> Enter inserts `\n` into the value buffer — the
        // tag check in dispatch_typed_keys's enter branch is what
        // splits the two paths. The form's submit handler must NOT
        // fire, otherwise authors couldn't type multi-paragraph
        // messages without accidentally posting.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var hits = 0;",
            "document.getElementById('f').addEventListener('submit', function(e) { hits = hits + 1; e.preventDefault(); });",
            "</script>",
            r#"<form id="f"><textarea id="t" name="msg" value="line1"></textarea></form>"#,
        ));
        browser.address_bar_focused = false;
        // last root = form, child 0 = textarea
        browser.focused_dom_path = Some(vec![0]);

        browser.apply_input(
            &input::WindowInput {
                enter_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        // Submit handler stayed silent.
        assert_eq!(browser.js.execute("hits").unwrap(), "0");
        // Value gained a trailing newline so the next typed char will
        // land on a new line.
        assert_eq!(
            browser.js.execute("document.getElementById('t').value").unwrap(),
            "\"line1\\n\""
        );
    }

    #[test]
    fn enter_in_textarea_fires_input_event_on_dirty_mutation() {
        // The newline insertion is a real value mutation, so the
        // `input` event must fire (same contract as a typed printable
        // character). A handler reading the new value should see the
        // trailing `\n` already there.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var seen = '';",
            "document.getElementById('t').addEventListener('input', function(e) { seen = e.target.value; });",
            "</script>",
            r#"<textarea id="t" value="ab"></textarea>"#,
        ));
        browser.address_bar_focused = false;
        browser.focused_dom_path = Some(vec![]);

        browser.apply_input(
            &input::WindowInput {
                enter_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.js.execute("seen").unwrap(), "\"ab\\n\"");
    }

    #[test]
    fn form_submit_collects_textarea_value_alongside_inputs() {
        // collect_form_data treats textareas the same as inputs: any
        // named field with a value lands in the URL-encoded query
        // string when the form GETs. This is what lets a contact form
        // with a name input and a message textarea actually submit
        // both fields.
        let mut browser = browser_with_html(concat!(
            "<script>",
            "var url = '';",
            "document.getElementById('f').addEventListener('submit', function(e) { url = 'submitted'; e.preventDefault(); });",
            "</script>",
            r#"<form id="f" action="/x"><input name="n" value="Alice"/><textarea name="m" value="hi"></textarea></form>"#,
        ));
        browser.address_bar_focused = false;
        // Focus the input and press Enter to trigger a submit (the
        // input path keeps its form-submit default action).
        browser.focused_dom_path = Some(vec![0]);

        browser.apply_input(
            &input::WindowInput {
                enter_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        // The handler ran (proves the form heard the submit) and the
        // textarea-bearing form is the one we configured. Empirically
        // proving the textarea's value rode along is a per-pair check
        // on the unit-test side (collect_form_data); here we only need
        // the integration smoke.
        assert_eq!(browser.js.execute("url").unwrap(), "\"submitted\"");
    }

    #[test]
    fn refresh_dispatches_load_off_main_thread_and_commits_on_a_later_frame() {
        // 5.8a: clicking refresh hands the blocking `load_remote_document`
        // call to the tokio spawn_blocking pool. The first click only flips
        // the browser into a `pending` state with a "loading…" status —
        // the new document only lands when a subsequent `display_list`
        // call observes the worker's result on the channel and commits it
        // through the same install_document funnel restore_entry uses.
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let body = "<div id=\"reloaded\">second</div>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let url = net::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        let mut browser = BrowserState::new(
            url.to_string(),
            "<div id=\"first\">first</div>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            Some(url),
            "loaded",
        );

        let refresh_rect = refresh_button_rect();
        browser.apply_input(
            &input::WindowInput {
                mouse_position: Some((refresh_rect.x + 2.0, refresh_rect.y + 2.0)),
                left_mouse_pressed: true,
                ..input::WindowInput::default()
            },
            800,
            600,
        );

        // The click only spawned the worker — document is still the old one.
        assert!(browser.has_pending_navigation());
        assert_eq!(browser.status_text, "loading…");
        assert!(browser.document_html.contains("first"));

        // Drive display_list frames until the pending slot clears. The
        // worker thread is on tokio's blocking pool so the resolve time
        // is bounded by the mock server's accept/respond — well under
        // the 1s budget below even on a loaded CI box.
        let mut frames = 0;
        while browser.has_pending_navigation() && frames < 200 {
            let _ = browser.display_list(800, 600, &input::WindowInput::default());
            if browser.has_pending_navigation() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            frames += 1;
        }

        assert!(
            !browser.has_pending_navigation(),
            "pending navigation should have resolved within {frames} frames"
        );
        assert!(
            browser.document_html.contains("reloaded"),
            "expected document to reflect server body, got: {}",
            browser.document_html
        );
        assert_eq!(browser.status_text, "loaded");

        server.join().unwrap();
    }

    #[test]
    fn refresh_click_while_pending_does_not_restart_loader() {
        // Reentrancy guard: a second refresh click while a worker is in
        // flight is a no-op. Without the guard the receiver field would
        // be replaced and the original worker's send would silently drop
        // its document; we'd also waste a network request.
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::mpsc,
            thread,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // The server sits on the connection until told to release, so
        // the test owns the timing window where `pending_navigation`
        // stays `Some` regardless of host load.
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let _ = release_rx.recv();
            let body = "<div id=\"reloaded\">second</div>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let url = net::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        let mut browser = BrowserState::new(
            url.to_string(),
            "<div id=\"first\">first</div>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            Some(url),
            "loaded",
        );

        let refresh_rect = refresh_button_rect();
        let refresh_input = input::WindowInput {
            mouse_position: Some((refresh_rect.x + 2.0, refresh_rect.y + 2.0)),
            left_mouse_pressed: true,
            ..input::WindowInput::default()
        };

        browser.apply_input(&refresh_input, 800, 600);
        assert!(browser.has_pending_navigation());
        assert_eq!(browser.status_text, "loading…");

        // Second click while the worker is still parked on the channel.
        // The status text is still "loading…" — the click is dropped
        // without touching state, so the assertion below is the same
        // value we observed after the first click.
        browser.apply_input(&refresh_input, 800, 600);
        assert!(browser.has_pending_navigation());
        assert_eq!(browser.status_text, "loading…");

        // Release the server so the worker can finish; drive frames
        // until the pending slot clears.
        release_tx.send(()).unwrap();
        let mut frames = 0;
        while browser.has_pending_navigation() && frames < 200 {
            let _ = browser.display_list(800, 600, &input::WindowInput::default());
            if browser.has_pending_navigation() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            frames += 1;
        }

        assert!(!browser.has_pending_navigation());
        assert!(browser.document_html.contains("reloaded"));
        server.join().unwrap();
    }

    #[test]
    fn raf_callback_dom_mutation_lands_in_browser_state_arena() {
        // rAF runs *before* the frame's layout pass, so any DOM mutation
        // it performs is visible to BrowserState's parsed_document arena
        // and therefore to whatever the layout/render pipeline reads next.
        let mut browser = browser_with_html_and_css(
            r#"<div id="host"></div><script>requestAnimationFrame(function () { var p = document.createElement('p'); p.textContent = 'rafted'; document.getElementById('host').appendChild(p); });</script>"#,
            "",
        );
        let _ = browser.display_list(800, 600, &input::WindowInput::default());
        let document = browser.parsed_document.borrow();
        let host = document.roots()[0];
        let host_kids = &document.get(host).unwrap().children;
        assert_eq!(host_kids.len(), 1);
        let p_kids = &document.get(host_kids[0]).unwrap().children;
        assert_eq!(p_kids.len(), 1);
        assert_eq!(document.text(p_kids[0]), Some("rafted"));
    }
}
