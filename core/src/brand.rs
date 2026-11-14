//! Product identity. Everything that names the app or its publisher reads
//! from here, so rebranding is a one-file change.

pub const PRODUCT_NAME: &str = "RustyDxf";
pub const VENDOR: &str = "art-tech";
/// Bundle identifier / application ID, shared by every platform.
pub const APP_ID: &str = "dev.art-tech.rustydxf";
/// Windows ProgID for the `.dxf` association.
pub const WINDOWS_PROG_ID: &str = "ArtTech.RustyDxf.Drawing";
pub const DESCRIPTION: &str = "A small, fast DXF viewer with a built-in MCP server.";
pub const COPYRIGHT: &str = "Copyright © 2026 art-tech. All rights reserved.";

/// The accent color (sRGB). Used by the UI highlights and the placeholder
/// icon; swap this for the art-tech brand color.
pub const ACCENT: [u8; 3] = [0xD9, 0x65, 0x2B];
