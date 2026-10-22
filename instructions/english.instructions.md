Guidelines about language usage apply both to generated assets as well as language used in the agent session itself, as well as associated temporary files and planning outputs.

# English

We use professional English in all communications and documentation. Avoid colloquialisms and informal language.

Use simple English, preferring everyday terms familiar to most readers with a high school education. Avoid jargon and complex sentence
structures whenever possible - not every reader may have English as their first language. Avoid introducing new terminology if it is not
necessary to distinguish important concepts from each other.

# Specialized terms must be distinctive

If different terms are used to distinguish different concepts, the terms should be distinctive (obviously referring to different concepts),
not merely linguistically different (synonyms). For example, using "car" and "automobile" to refer to the different concepts is not
distinctive because they are essentially synonyms whereas a "cargo vehicle" and "passenger vehicle" are distinctive due to the extra qualifiers.

Differences between similar terms must be documented in some relevant location (e.g. in the API documentation page that introduces the terms).

# Specialized terms must carry weight

Avoid defining specialized terminology if the distinctions between terms do not play a critical role in the text or discussion. For
terms of minor significance, prefer using common language with appropriate qualifiers. For example, instead of defining a taxonomy of
"incident", "problem", "finding" just for the sake of two paragraphs, speak of "problems with shared root causes", "workflow failures"
and "issues filed for workflow failures". Defining specialized terms only makes sense if they are cross-referenced from many places
and the distinction becomes important throughout a larger text.

Conversely, when specialized terms are defined they must actually be used, instead of using free-form language that ignores the established
distinctions. For example, if you do define "incident", "problem", and "finding" as specialized terms, you must consistently use them
according to their defined meanings throughout the text, instead of the looser "problems with shared root causes" style wording. Pick one
pattern and stick to it.

# Common terminology practices to avoid

Do not use the word "seam" when you can use a more typical term like "abstraction", "concept" or "API" instead.

# Avoid counting list items in prose

Do not mention the count of list items in prose - the user can do their own counting if they want to.

Good example: "The following items are available:"
Bad example: "There are 3 items available:"

# Avoid listing if a blanket statement is sufficient

Listings are for emphasis but if there is no emphasis needed, apply a broad stroke to say more with less.

Good example: "Do not copy any other files into the output directory"
Bad example: "Do not copy build files, test files, packaging logs, configuration files or other unwanted files into the output directory"

# You can add words to the dictionary

Our spell checkers are not always correct. If they reject valid words, just add these words to the project's dictionary file if one exists.

# Avoid jargon

Do not introduce new terms to text unless it is impossible to express something with plain English and already-established context-specific terminology.
Prefer compound terms that are already widely understood over introducing new jargon (e.g. "a handle that has not yet been cleaned up" instead of "retained handle").

# A reader should not need to reverse engineer

Neither developer documentation nor public API documenation nor private API documentation should require the user to reverse engineer code to
understand what it is talking about. Description must be contextualized, terms must be defined or their definitions available in referenced context,
jargon must be avoided, clear and understandable English must be used. Consider who the reader is and what they may or may not know already, do not
assume a reader has all the knowledge that you already have.