//! PowerPoint presentations (PPTX), with their SmartArt graphics and charts.

use anyhow::{Context, bail};
use quick_xml::events::Event;
use std::collections::HashMap;
use std::path::Path;
use super::narration::with_stop;
use super::ppt::{is_compound_file, read_ppt};
use super::xml::{MAX_XML_BYTES, attr, open_zip, prefixed_attr, push_ref, read_zip_entry, resolve_part};

/// Reads each slide of a PowerPoint presentation in turn, in the order they
/// are shown, introducing each one with its number. The text of SmartArt
/// graphics and the data of charts are read where they appear. Speaker notes
/// and comments are left out, as comments are for DOCX.
pub(super) fn read_pptx(path: &Path) -> anyhow::Result<String> {
    if is_compound_file(path)? {
        // Office keeps a password-protected PPTX inside a compound file; any
        // other compound file is a PPT that has been given a .pptx name.
        let file = cfb::open(path).context("this is not a valid PPTX file")?;
        if file.exists("EncryptedPackage") {
            bail!("the presentation is password protected");
        }
        return read_ppt(path);
    }
    let mut archive = open_zip(path, "PPTX")?;
    let mut budget = MAX_XML_BYTES;
    let presentation = read_zip_entry(&mut archive, "ppt/presentation.xml", "PPTX", &mut budget)?;
    let rels = read_zip_entry(&mut archive, "ppt/_rels/presentation.xml.rels", "PPTX", &mut budget)?;
    let targets = parse_rels(&rels)?;
    let mut slides = Vec::new();
    for id in pptx_slide_ids(&presentation)? {
        // A missing slide still counts, so the slides after it keep their
        // numbers.
        let name = targets.get(&id).map(|t| resolve_part("ppt", t));
        let text = match name {
            Some(name) if archive.index_for_name(&name).is_some() => {
                let xml = read_zip_entry(&mut archive, &name, "PPTX", &mut budget)?;
                read_pptx_slide(&mut archive, &name, &xml, &mut budget)?
            }
            _ => {
                log::warn!("PPTX slide {id} is missing");
                String::new()
            }
        };
        slides.push(text);
    }
    Ok(speak_slides(slides))
}

/// Reads the slide `name`, whose XML is `xml`, following its links to the
/// charts and SmartArt graphics it shows.
fn read_pptx_slide(
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
    xml: &str,
    budget: &mut u64,
) -> anyhow::Result<String> {
    let (folder, file) = name.rsplit_once('/').unwrap_or(("", name));
    let rels_name = format!("{folder}/_rels/{file}.rels");
    let targets = match archive.index_for_name(&rels_name) {
        Some(_) => parse_rels(&read_zip_entry(archive, &rels_name, "PPTX", budget)?)?,
        None => HashMap::new(),
    };
    pptx_slide_to_text(xml, |embedded| {
        let (id, is_chart) = match &embedded {
            Embedded::Chart(id) => (id, true),
            Embedded::Diagram(id) => (id, false),
        };
        let Some(part) = targets.get(id).map(|t| resolve_part(folder, t)) else { return Ok(String::new()) };
        if archive.index_for_name(&part).is_none() {
            log::warn!("PPTX part {part} is missing");
            return Ok(String::new());
        }
        let xml = read_zip_entry(archive, &part, "PPTX", budget)?;
        if is_chart { chart_xml_to_text(&xml) } else { diagram_xml_to_text(&xml) }
    })
}

/// The relationship ids of the slides listed in `ppt/presentation.xml`, in
/// the order they are shown.
fn pptx_slide_ids(xml: &str) -> anyhow::Result<Vec<String>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut ids = Vec::new();
    loop {
        match reader.read_event().context("the PPTX presentation is damaged")? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "sldId" => {
                // The slide's own `id` has no prefix; the relationship id does.
                if let Some(id) = prefixed_attr(&e, "id") {
                    ids.push(id);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(ids)
}

/// Maps each relationship id in a `.rels` part to its target.
fn parse_rels(xml: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut targets = HashMap::new();
    loop {
        match reader.read_event().context("the PPTX presentation is damaged")? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" => {
                if let (Some(id), Some(target)) = (attr(&e, "Id"), attr(&e, "Target")) {
                    targets.insert(id, target);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(targets)
}

/// A chart or SmartArt graphic on a slide, kept in another part of the file
/// and named by its relationship id.
enum Embedded {
    Chart(String),
    Diagram(String),
}

/// Pulls the text out of a PresentationML slide, one paragraph per text
/// paragraph, in the order the shapes appear on the slide. The text of each
/// chart or SmartArt graphic comes from `embedded`.
fn pptx_slide_to_text(xml: &str, mut embedded: impl FnMut(Embedded) -> anyhow::Result<String>) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text = false;
    // The shape is a date, footer or slide number placeholder, which repeats
    // on every slide, so its text is left out.
    let mut skip_shape = false;
    // How many `mc:Fallback` elements the reader is inside. Newer content,
    // such as Office 2016 charts, comes with a fallback copy for older apps
    // (for those charts, a box saying the chart can't be shown), which is
    // left out so it isn't read as well.
    let mut fallback = 0usize;
    loop {
        let event = reader.read_event().context("a PPTX slide is damaged")?;
        match &event {
            Event::Start(e) if e.local_name().as_ref() == "Fallback" => fallback += 1,
            Event::End(e) if e.local_name().as_ref() == "Fallback" => fallback = fallback.saturating_sub(1),
            Event::Eof => break,
            _ if fallback > 0 => continue,
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "ph" => {
                if matches!(attr(e, "type").as_deref(), Some("dt" | "ftr" | "sldNum" | "hdr")) {
                    skip_shape = true;
                }
            }
            Event::Start(e) | Event::Empty(e) if matches!(e.local_name().as_ref(), "chart" | "relIds") => {
                let found = match e.local_name().as_ref() {
                    "chart" => prefixed_attr(e, "id").map(Embedded::Chart),
                    _ => prefixed_attr(e, "dm").map(Embedded::Diagram),
                };
                if let Some(found) = found {
                    out.push_str(&embedded(found)?);
                    out.push_str("\n\n");
                }
            }
            Event::Start(e) if e.local_name().as_ref() == "sp" => skip_shape = false,
            Event::Start(e) if e.local_name().as_ref() == "t" => in_text = true,
            Event::Empty(e) if !skip_shape => match e.local_name().as_ref() {
                "br" => out.push('\n'),
                "tab" => out.push('\t'),
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_text = false,
                "p" => out.push_str("\n\n"),
                "sp" => skip_shape = false,
                _ => {}
            },
            Event::Text(t) if in_text && !skip_shape => out.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_text && !skip_shape => push_ref(&mut out, r),
            _ => {}
        }
    }
    Ok(out)
}

/// Reads the text of a SmartArt graphic from its data part
/// (`ppt/diagrams/dataN.xml`), one paragraph per box, in the order of the
/// graphic's outline: each box, then the boxes under it.
fn diagram_xml_to_text(xml: &str) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    // Each box's id and text, in the order written, and the id of the
    // graphic as a whole.
    let mut points: Vec<(String, String)> = Vec::new();
    let mut root = None;
    // The parent of each box, and its children with their positions.
    let mut children: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    let mut point: Option<(String, String)> = None;
    let mut in_text = false;
    loop {
        match reader.read_event().context("a SmartArt graphic is damaged")? {
            Event::Start(e) => {
                let name = e.local_name();
                in_text = name.as_ref() == "t";
                match name.as_ref() {
                    "pt" => {
                        let id = attr(&e, "modelId").unwrap_or_default();
                        match attr(&e, "type").as_deref() {
                            None | Some("node" | "asst") => point = Some((id, String::new())),
                            Some("doc") => root = Some(id),
                            // Connector shapes and layout points hold no text.
                            _ => {}
                        }
                    }
                    "cxn" => add_connection(&e, &mut children),
                    _ => {}
                }
            }
            Event::Empty(e) => match e.local_name().as_ref() {
                "cxn" => add_connection(&e, &mut children),
                "br" | "tab" => {
                    if let Some((_, text)) = &mut point {
                        text.push(' ');
                    }
                }
                _ => {}
            },
            Event::End(e) => {
                in_text = false;
                match e.local_name().as_ref() {
                    "p" => {
                        if let Some((_, text)) = &mut point {
                            text.push_str("\n\n");
                        }
                    }
                    "pt" => points.extend(point.take()),
                    _ => {}
                }
            }
            Event::Text(t) if in_text => {
                if let Some((_, text)) = &mut point {
                    text.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) if in_text => {
                if let Some((_, text)) = &mut point {
                    push_ref(text, &r);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    // Walk the outline from the top. Boxes it doesn't reach, which a damaged
    // file might have, are read at the end in the order written.
    for kids in children.values_mut() {
        kids.sort();
    }
    let mut order: Vec<&str> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<&str> = root.as_deref().into_iter().collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        order.push(id);
        if let Some(kids) = children.get(id) {
            stack.extend(kids.iter().rev().map(|(_, kid)| kid.as_str()));
        }
    }
    let texts: HashMap<&str, &str> = points.iter().map(|(id, text)| (id.as_str(), text.as_str())).collect();
    let mut out = String::new();
    let unreached = points.iter().map(|(id, _)| id.as_str()).filter(|id| !seen.contains(id));
    for id in order.into_iter().chain(unreached) {
        if let Some(text) = texts.get(id) {
            out.push_str(text);
            out.push_str("\n\n");
        }
    }
    Ok(out)
}

/// Records a SmartArt connection between a box and the box under it. Other
/// kinds of connection link boxes to how they are drawn, and are ignored.
fn add_connection(e: &quick_xml::events::BytesStart, children: &mut HashMap<String, Vec<(usize, String)>>) {
    if attr(e, "type").is_some_and(|t| t != "parOf") {
        return;
    }
    if let (Some(parent), Some(child)) = (attr(e, "srcId"), attr(e, "destId")) {
        let position = attr(e, "srcOrd").and_then(|o| o.trim().parse().ok()).unwrap_or(usize::MAX);
        children.entry(parent).or_default().push((position, child));
    }
}

/// One series of a chart: its name, and its categories and values by index.
#[derive(Default)]
struct ChartSeries {
    name: String,
    categories: std::collections::BTreeMap<usize, String>,
    values: std::collections::BTreeMap<usize, String>,
    /// The number format of the values, such as "0%".
    format: String,
}

/// Reads a chart (`ppt/charts/chartN.xml`) as speech: its title and axis
/// titles, then each series in turn with the value for each category, from
/// the copy of the data the chart keeps with it.
fn chart_xml_to_text(xml: &str) -> anyhow::Result<String> {
    /// Elements whose text is not part of the chart's data or titles: data
    /// labels, trendlines, error bars and extensions.
    const SKIPPED: [&str; 5] = ["extLst", "dLbls", "dLbl", "trendline", "errBars"];
    let mut reader = quick_xml::Reader::from_str(xml);
    // The local names of the elements the reader is inside.
    let mut stack: Vec<String> = Vec::new();
    let mut title = String::new();
    let mut axis_titles: Vec<String> = Vec::new();
    let mut series: Vec<ChartSeries> = Vec::new();
    let mut index = 0usize;
    loop {
        let text = match reader.read_event().context("a chart is damaged")? {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_owned();
                match name.as_str() {
                    "ser" => series.push(ChartSeries::default()),
                    "title" if stack.last().is_some_and(|p| p != "chart") => axis_titles.push(String::new()),
                    "pt" => index = attr(&e, "idx").and_then(|i| i.trim().parse().ok()).unwrap_or(0),
                    _ => {}
                }
                stack.push(name);
                continue;
            }
            Event::End(_) => {
                // Paragraphs of a title run on as one line.
                if stack.pop().as_deref() == Some("p") && !stack.iter().any(|n| n == "ser") {
                    match stack.iter().rposition(|n| n == "title") {
                        Some(at) if at > 0 && stack[at - 1] == "chart" => title.push(' '),
                        Some(_) => axis_titles.last_mut().into_iter().for_each(|t| t.push(' ')),
                        None => {}
                    }
                }
                continue;
            }
            Event::Text(t) => t.xml10_content().into_owned(),
            Event::GeneralRef(r) => {
                let mut s = String::new();
                push_ref(&mut s, &r);
                s
            }
            Event::Eof => break,
            _ => continue,
        };
        let Some(leaf) = stack.last().map(String::as_str) else { continue };
        if !matches!(leaf, "t" | "v" | "formatCode") || stack.iter().any(|n| SKIPPED.contains(&n.as_str())) {
            continue;
        }
        if let Some(ser) = stack.iter().position(|n| n == "ser") {
            let Some(current) = series.last_mut() else { continue };
            match (stack.get(ser + 1).map(String::as_str), leaf) {
                (Some("tx"), "t" | "v") => current.name.push_str(&text),
                (Some("cat" | "xVal"), "v") => {
                    let category = current.categories.entry(index).or_default();
                    // Categories with more than one level, such as a quarter
                    // within a year, give each level in turn.
                    if !category.is_empty() {
                        category.push_str(", ");
                    }
                    category.push_str(text.trim());
                }
                (Some("val" | "yVal"), "v") => {
                    current.values.insert(index, text.trim().to_owned());
                }
                (Some("val" | "yVal"), "formatCode") => current.format.push_str(&text),
                _ => {}
            }
        } else if leaf != "formatCode"
            && let Some(at) = stack.iter().rposition(|n| n == "title")
        {
            if at > 0 && stack[at - 1] == "chart" {
                title.push_str(&text);
            } else if let Some(axis) = axis_titles.last_mut() {
                axis.push_str(&text);
            }
        }
    }
    Ok(speak_chart(&title, &axis_titles, &series))
}

/// Puts a chart into words: "Chart: Sales by region. Axes: Quarter, Sales
/// (£m)." then a paragraph for each series, such as "North. Q1: 10. Q2: 20."
fn speak_chart(title: &str, axis_titles: &[String], series: &[ChartSeries]) -> String {
    let tidy_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = tidy_line(title);
    let mut out = if title.is_empty() { "Chart.".to_owned() } else { format!("Chart: {}", with_stop(&title)) };
    let axes: Vec<String> = axis_titles.iter().map(|t| tidy_line(t)).filter(|t| !t.is_empty()).collect();
    if !axes.is_empty() {
        out.push_str(&format!(" {}: {}", if axes.len() == 1 { "Axis" } else { "Axes" }, with_stop(&axes.join(", "))));
    }
    for (n, ser) in series.iter().enumerate() {
        let name = tidy_line(&ser.name);
        let name = if name.is_empty() { format!("Series {}", n + 1) } else { name };
        out.push_str(&format!("\n\n{}", with_stop(&name)));
        for (i, value) in &ser.values {
            let category = ser.categories.get(i).map(|c| tidy_line(c)).filter(|c| !c.is_empty());
            let category = category.unwrap_or_else(|| format!("Point {}", i + 1));
            out.push_str(&format!(" {category}: {}.", speakable_number(value, &ser.format)));
        }
    }
    out
}

/// Writes a chart value as it would be read: "0.35" in a percentage format
/// is "35%", and the rounding noise in values such as "12.300000000000001"
/// is dropped. Values that aren't numbers are kept as they are.
fn speakable_number(value: &str, format: &str) -> String {
    let Ok(mut n) = value.trim().parse::<f64>() else { return value.trim().to_owned() };
    if !n.is_finite() {
        return value.trim().to_owned();
    }
    let percent = format.contains('%');
    if percent {
        n *= 100.0;
    }
    let mut s = format!("{n:.6}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    if s == "-0" {
        s = "0".to_owned();
    }
    if percent { format!("{s}%") } else { s }
}

/// Joins the text of each slide, introducing each with its number. Slides
/// with no text are left out, but the others keep their own numbers.
pub(super) fn speak_slides(slides: Vec<String>) -> String {
    let slides: Vec<String> = slides
        .into_iter()
        .enumerate()
        .filter(|(_, text)| !text.trim().is_empty())
        .map(|(n, text)| format!("Slide {}.\n\n{}", n + 1, text.trim()))
        .collect();
    slides.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{DocumentReading, extract_text, tidy};

    #[test]
    fn pptx_reads_slides_in_show_order() {
        use std::io::Write;
        let file = tempfile::Builder::new().suffix(".pptx").tempfile().unwrap();
        let mut zip = zip::ZipWriter::new(file.reopen().unwrap());
        let parts = [
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/><p:sldId id="259" r:id="rId9"/><p:sldId id="258" r:id="rId4"/><p:sldId id="260" r:id="rId5"/></p:sldIdLst></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Target="slides/slide1.xml"/><Relationship Id="rId3" Target="/ppt/slides/slide2.xml"/><Relationship Id="rId4" Target="slides/../slides/slide3.xml"/><Relationship Id="rId5" Target="slides/slide4.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Fish &amp; chips</a:t></a:r><a:br/><a:r><a:t>today</a:t></a:r></a:p><a:p><a:r><a:t>Second</a:t></a:r><a:r><a:t> point</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/slide2.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Welcome</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:nvPr><p:ph type="sldNum" idx="12"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:fld type="slidenum"><a:t>1</a:t></a:fld></a:p></p:txBody></p:sp><p:sp><p:txBody><a:p><a:r><a:t>After</a:t></a:r></a:p></p:txBody></p:sp></p:sld>"#,
            ),
            ("ppt/slides/slide3.xml", r#"<p:sld xmlns:p="p" xmlns:a="a"><a:p><a:endParaRPr/></a:p></p:sld>"#),
            (
                "ppt/slides/slide4.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><a:p><a:r><a:t>Last</a:t></a:r></a:p><p:graphicFrame><a:graphic><a:graphicData><c:chart xmlns:c="c" r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame><p:graphicFrame><a:graphic><a:graphicData><dgm:relIds xmlns:dgm="d" r:dm="rId3" r:lo="rId4"/></a:graphicData></a:graphic></p:graphicFrame><p:graphicFrame><a:graphic><a:graphicData><c:chart xmlns:c="c" r:id="rId8"/></a:graphicData></a:graphic></p:graphicFrame></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide4.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Target="../charts/chart1.xml"/><Relationship Id="rId3" Target="../diagrams/data1.xml"/><Relationship Id="rId8" Target="../charts/missing.xml"/></Relationships>"#,
            ),
            (
                "ppt/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Sales</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:pieChart><c:ser><c:tx><c:v>North</c:v></c:tx><c:cat><c:strLit><c:pt idx="0"><c:v>Q1</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:pt idx="0"><c:v>10</c:v></c:pt></c:numLit></c:val></c:ser></c:pieChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
            (
                "ppt/diagrams/data1.xml",
                r#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a"><dgm:ptLst><dgm:pt modelId="0" type="doc"/><dgm:pt modelId="1"><dgm:t><a:p><a:r><a:t>Step</a:t></a:r></a:p></dgm:t></dgm:pt></dgm:ptLst><dgm:cxnLst><dgm:cxn modelId="9" srcId="0" destId="1" srcOrd="0" destOrd="0"/></dgm:cxnLst></dgm:dataModel>"#,
            ),
        ];
        for (name, xml) in parts {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        assert_eq!(
            extract_text(file.path(), DocumentReading::Content).unwrap(),
            "Slide 1.\n\nWelcome\n\nAfter\n\nSlide 2.\n\nFish & chips today\n\nSecond point\n\nSlide 5.\n\nLast\n\nChart: Sales.\n\nNorth. Q1: 10.\n\nStep"
        );
    }

    #[test]
    fn password_protected_presentations_say_so() {
        for (suffix, stream) in [(".pptx", "EncryptedPackage"), (".ppt", "EncryptedSummary")] {
            let file = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
            let mut cfb = cfb::create(file.path()).unwrap();
            cfb.create_stream(stream).unwrap();
            cfb.flush().unwrap();
            drop(cfb);
            let error = extract_text(file.path(), DocumentReading::Content).unwrap_err().to_string();
            assert!(error.contains("password protected"), "{suffix}: {error}");
        }
    }

    #[test]
    fn chart_reads_titles_and_each_series() {
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart>
            <c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>Sales by</a:t></a:r><a:r><a:t> region</a:t></a:r></a:p><a:p><a:r><a:t>2026</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/></c:title>
            <c:plotArea><c:barChart><c:barDir val="col"/>
                <c:ser><c:idx val="0"/><c:order val="0"/>
                    <c:tx><c:strRef><c:f>Sheet1!$B$1</c:f><c:strCache><c:ptCount val="1"/><c:pt idx="0"><c:v>North &amp; East</c:v></c:pt></c:strCache></c:strRef></c:tx>
                    <c:dLbls><c:dLbl><c:idx val="0"/><c:tx><c:rich><a:p><a:r><a:t>label</a:t></a:r></a:p></c:rich></c:tx></c:dLbl></c:dLbls>
                    <c:cat><c:multiLvlStrRef><c:f>Sheet1!$A$2:$A$3</c:f><c:multiLvlStrCache><c:ptCount val="2"/>
                        <c:lvl><c:pt idx="0"><c:v>Q1</c:v></c:pt><c:pt idx="1"><c:v>Q2</c:v></c:pt></c:lvl>
                        <c:lvl><c:pt idx="0"><c:v>2026</c:v></c:pt></c:lvl>
                    </c:multiLvlStrCache></c:multiLvlStrRef></c:cat>
                    <c:val><c:numRef><c:f>Sheet1!$B$2:$B$3</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val="2"/><c:pt idx="0"><c:v>12.300000000000001</c:v></c:pt><c:pt idx="1"><c:v>20</c:v></c:pt></c:numCache></c:numRef></c:val>
                    <c:extLst><c:ext><c15:filteredSeriesTitle><c15:tx><c:v>hidden</c:v></c15:tx></c15:filteredSeriesTitle></c:ext></c:extLst>
                </c:ser>
                <c:ser><c:idx val="1"/>
                    <c:val><c:numRef><c:numCache><c:formatCode>0.0%</c:formatCode><c:pt idx="1"><c:v>0.355</c:v></c:pt></c:numCache></c:numRef></c:val>
                </c:ser>
            </c:barChart>
            <c:catAx><c:axId val="1"/><c:title><c:tx><c:rich><a:p><a:r><a:t>Quarter</a:t></a:r></a:p></c:rich></c:tx></c:title></c:catAx>
            <c:valAx><c:axId val="2"/><c:title><c:tx><c:rich><a:p><a:r><a:t>Sales (£m)</a:t></a:r></a:p></c:rich></c:tx></c:title><c:txPr><a:p><a:pPr/></a:p></c:txPr></c:valAx>
            </c:plotArea></c:chart></c:chartSpace>"#;
        assert_eq!(
            tidy(&chart_xml_to_text(xml).unwrap()),
            "Chart: Sales by region 2026. Axes: Quarter, Sales (£m).\n\n\
             North & East. Q1, 2026: 12.3. Q2: 20.\n\n\
             Series 2. Point 2: 35.5%."
        );
        assert_eq!(chart_xml_to_text("<c:chartSpace/>").unwrap(), "Chart.");
    }

    #[test]
    fn chart_numbers_read_naturally() {
        assert_eq!(speakable_number("12.300000000000001", "General"), "12.3");
        assert_eq!(speakable_number("1500", ""), "1500");
        assert_eq!(speakable_number("0.25", "0%"), "25%");
        assert_eq!(speakable_number("-0.0000001", ""), "0");
        assert_eq!(speakable_number(" n/a ", ""), "n/a");
    }

    #[test]
    fn smartart_reads_boxes_in_outline_order() {
        let xml = r#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a"><dgm:ptLst>
            <dgm:pt modelId="{0}" type="doc"><dgm:prSet/><dgm:spPr/><dgm:t><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr/></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{B}"><dgm:prSet phldrT="[Text]"/><dgm:spPr/><dgm:t><a:bodyPr/><a:p><a:r><a:t>Second</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{A}"><dgm:t><a:p><a:r><a:t>First &amp; foremost</a:t></a:r><a:br/><a:r><a:t>line two</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{A1}"><dgm:t><a:p><a:r><a:t>Under first</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{T}" type="parTrans" cxnId="{c1}"><dgm:t><a:p><a:r><a:t>connector</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{P}" type="pres"><dgm:prSet presName="x"/></dgm:pt>
            <dgm:pt modelId="{O}"><dgm:t><a:p><a:r><a:t>Orphan</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{E}"><dgm:t><a:p><a:endParaRPr/></a:p></dgm:t></dgm:pt>
        </dgm:ptLst><dgm:cxnLst>
            <dgm:cxn modelId="{c1}" srcId="{0}" destId="{B}" srcOrd="1" destOrd="0"/>
            <dgm:cxn modelId="{c2}" srcId="{0}" destId="{A}" srcOrd="0" destOrd="0"/>
            <dgm:cxn modelId="{c3}" srcId="{A}" destId="{A1}" srcOrd="0" destOrd="0"/>
            <dgm:cxn modelId="{c4}" type="presOf" srcId="{B}" destId="{P}" srcOrd="0" destOrd="0"/>
            <dgm:cxn modelId="{c5}" srcId="{A1}" destId="{A}" srcOrd="0" destOrd="0"/>
        </dgm:cxnLst></dgm:dataModel>"#;
        assert_eq!(
            tidy(&diagram_xml_to_text(xml).unwrap()),
            "First & foremost line two\n\nUnder first\n\nSecond\n\nOrphan"
        );
    }

    #[test]
    fn pptx_slide_skips_fallback_copies() {
        let xml = r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:mc="mc" xmlns:r="r"><mc:AlternateContent>
            <mc:Choice Requires="cx1"><p:graphicFrame><a:graphic><a:graphicData><cx:chart xmlns:cx="cx" r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame></mc:Choice>
            <mc:Fallback><p:sp><p:txBody><a:p><a:r><a:t>This chart isn't available in your version of PowerPoint</a:t></a:r></a:p></p:txBody></p:sp></mc:Fallback>
        </mc:AlternateContent><a:p><a:r><a:t>After</a:t></a:r></a:p></p:sld>"#;
        let text = pptx_slide_to_text(xml, |e| Ok(if matches!(e, Embedded::Chart(id) if id == "rId2") { "Chart." } else { "" }.into()));
        assert_eq!(tidy(&text.unwrap()), "Chart.\n\nAfter");
    }
}
