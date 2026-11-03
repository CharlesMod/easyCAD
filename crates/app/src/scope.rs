//! Editing inside a component. The feature dialogs, sketch mode, history strip and
//! Parameters panel work on a `DocData` view of the active component
//! (`DocData::component_view`: its sketches, features and imports, the document
//! params with its local params on top). Edits go through [`edit`], which runs the
//! change on that view inside one undoable document edit and writes the
//! component's content back.

use easycad_model::assembly::ComponentId;
use easycad_model::document::{DocData, DocError, Document};
use easycad_model::feature::Feature;

/// The document as one component sees it (`ROOT` = the document itself).
pub fn view(d: &DocData, c: ComponentId) -> DocData {
    if c == ComponentId::ROOT {
        return d.clone();
    }
    d.component_view(c).unwrap_or_else(|| d.clone())
}

/// The feature list of a component.
pub fn features_mut(d: &mut DocData, c: ComponentId) -> Option<&mut Vec<Feature>> {
    if c == ComponentId::ROOT {
        Some(&mut d.features)
    } else {
        d.component_mut(c).map(|c| &mut c.features)
    }
}

pub fn features(d: &DocData, c: ComponentId) -> &[Feature] {
    if c == ComponentId::ROOT {
        &d.features
    } else {
        d.component(c).map(|c| c.features.as_slice()).unwrap_or(&[])
    }
}

/// One undoable edit of a component's content through its view. Params changed on
/// the view are ignored (use `Document::set_component_param`).
pub fn edit<T>(
    doc: &mut Document,
    c: ComponentId,
    label: &str,
    f: impl FnOnce(&mut DocData) -> Result<T, DocError>,
) -> Result<T, DocError> {
    if c == ComponentId::ROOT {
        return doc.edit(label, f);
    }
    doc.edit(label, |d| {
        let comp = d
            .component(c)
            .ok_or_else(|| DocError::Other("the component being edited is gone".into()))?;
        if comp.locked {
            return Err(DocError::Other(format!(
                "`{}` is locked (break the link to edit it)",
                comp.name
            )));
        }
        let mut v = d
            .component_view(c)
            .ok_or_else(|| DocError::Other("the component being edited is gone".into()))?;
        let r = f(&mut v)?;
        let comp = d.component_mut(c).expect("checked above");
        comp.sketches = v.sketches;
        comp.features = v.features;
        comp.imports = v.imports;
        comp.rollback = v.rollback;
        d.meta = v.meta;
        d.next_id = d.next_id.max(v.next_id);
        Ok(r)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use easycad_model::assembly::Transform;

    #[test]
    fn edits_land_in_the_component() {
        let mut doc = Document::new("asm");
        doc.set_param("wall", "2").unwrap();
        let c = doc.add_component("Lid");
        doc.add_occurrence(ComponentId::ROOT, c, "Lid:1", Transform::IDENTITY)
            .unwrap();
        doc.set_component_param(c, "t", "wall * 2").unwrap();
        let v = view(&doc.data, c);
        assert!(v.params.get("wall").is_some() && v.params.get("t").is_some());
        let sk = easycad_model::sketch::Sketch::new("S", easycad_model::geom::Plane::XY);
        edit(&mut doc, c, "Add sketch", |d| {
            d.add_sketch(sk);
            Ok(())
        })
        .unwrap();
        assert_eq!(doc.data.component(c).unwrap().sketches.len(), 1);
        assert!(doc.data.sketches.is_empty());
        assert_eq!(doc.undo_label(), Some("Add sketch"));
        doc.undo();
        assert!(doc.data.component(c).unwrap().sketches.is_empty());
    }

    #[test]
    fn rollback_markers_are_per_component() {
        let mut doc = crate::sample::sensor_node();
        let n = doc.data.features.len();
        doc.set_rollback(Some(2));
        let c = doc.add_component("Part");
        doc.add_occurrence(ComponentId::ROOT, c, "Part:1", Transform::IDENTITY)
            .unwrap();
        // the engine keeps the root's marker out of component views (GUI_NEEDS #20)
        let eff = crate::adapters::effective_in(&doc.data, ComponentId::ROOT, None);
        assert_eq!(eff.rollback, Some(2));
        assert_eq!(eff.component_view(c).unwrap().rollback, None);
        // a component's own marker is saved in the document and undoable
        doc.set_component_rollback(c, None).unwrap();
        assert_eq!(doc.data.component(c).unwrap().rollback, None);
        assert_eq!(doc.data.features.len(), n);
    }
}
