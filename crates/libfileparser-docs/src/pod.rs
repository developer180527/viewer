//! POD (Perl's documentation) and Fountain (screenplays), read by `markup-lite`
//! into HTML, then formatted text (`crate::markup`).

use libfileparser_core::Result;

use crate::markup::{Rendered, markup};

markup!(
    /// POD, in a `.pod` file of its own.
    PodReader, POD, "pod", "POD", "Perl documentation", ["pod"], ["text/x-pod"], pod
);

markup!(
    /// Fountain, the plain-text screenplay format: scenes, characters and their
    /// dialogue, parentheticals, transitions, sections, synopses and notes.
    FountainReader, FOUNTAIN, "fountain", "Fountain", "Screenplay", ["fountain", "spmd"], ["text/x-fountain"], fountain
);

fn pod(text: &str) -> Result<Rendered> {
    Ok(markup_lite::pod(text).into())
}

fn fountain(text: &str) -> Result<Rendered> {
    Ok(markup_lite::fountain(text).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::{Content, Document, FileSource, ParseContext, Reader};

    fn blocks(reader: &dyn Reader, name: &str, text: &str) -> (Document, String) {
        let doc = reader.open(&FileSource::from_bytes(name, text.as_bytes().to_vec()), &ParseContext::new()).unwrap();
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..1000).unwrap());
        (doc, s)
    }

    fn has(s: &str, wants: &[&str]) {
        for w in wants {
            assert!(s.contains(w), "{w} in {s}");
        }
    }

    #[test]
    fn pod_document() {
        let (doc, s) = blocks(&PodReader, "a.pod", "ignored before pod\n\n=head1 NAME\n\nFoo::Bar - does things\n\n=head1 SYNOPSIS\n\n  use Foo::Bar;\n  my $x = Foo::Bar->new;\n\n=head2 Methods\n\nCall B<new> with I<< args >> and C<< $x->go >>, see L<perlpod> and L<the site|https://x.org>. E<lt>tagE<gt>\n\n=over 4\n\n=item * first\n\n=item * second\n\n=back\n\n=over\n\n=item new\n\nMakes one.\n\n=back\n\n=begin html\n\n<b>raw</b>\n\n=end html\n\n=cut\n\nsub code {}\n");
        has(&s, &["Heading { level: 1, text: [Text(\"NAME\")]", "Foo::Bar - does things", "use Foo::Bar;\\n  my $x", "Strong([Text(\"new\")])", "Emphasis([Text(\"args\")])", "Code(\"$x->go\")", "perldoc:perlpod", "https://x.org", "<tag>", "List { ordered: false", "second", "Strong([Text(\"new\")])", "Makes one."]);
        for not in ["ignored before", "raw", "sub code"] {
            assert!(!s.contains(not), "{not} in {s}");
        }
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Foo::Bar"));
    }

    #[test]
    fn fountain_screenplay() {
        let (doc, s) = blocks(&FountainReader, "a.fountain", "Title: **BRICK & STEEL**\nCredit: Written by\nAuthor: Stu Maschwitz\n\n# Act One\n\nINT. GARAGE - DAY #1#\n\nBRICK and STEEL sit in the *dark*.\n\nSTEEL (V.O.)\n(quietly)\nBeer's ready!\nAre they cold?\n\n[[A note to cut]]\n\nCUT TO:\n\n.FLASHBACK\n\n> THE END <\n\n= A synopsis.\n");
        has(&s, &["Heading { level: 1, text: [Strong([Text(\"BRICK & STEEL\")])]", "Stu Maschwitz", "Heading { level: 2, text: [Text(\"Act One\")]", "Heading { level: 3, text: [Text(\"1 INT. GARAGE - DAY\")]", "Emphasis([Text(\"dark\")])", "Strong([Text(\"STEEL (V.O.)\")])", "Quote([Paragraph([Emphasis([Text(\"(quietly)\")])]), Paragraph([Text(\"Beer's ready!\"), LineBreak, Text(\"Are they cold?\")])", "Strong([Text(\"CUT TO:\")])", "FLASHBACK", "THE END", "A synopsis."]);
        assert!(!s.contains("note to cut"), "{s}");
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "BRICK & STEEL"));
    }
}
