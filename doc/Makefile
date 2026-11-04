all: manual.pdf

CHAPTERS := $(wildcard chapters/*.tex)

manual.pdf: manual.tex $(CHAPTERS)
	pdflatex -interaction=nonstopmode manual.tex
	pdflatex -interaction=nonstopmode manual.tex

clean:
	rm -f manual.aux manual.log manual.out manual.toc
	rm -f chapters/*.aux
	rm -f manual.pdf

.PHONY: all clean
